//! The instance supervisor (imperative shell, design D3): spawns one task per
//! fault instance, executes the effects decided by [`crate::machine`], and
//! keeps the per-instance view the reconciliation report is built from.
//!
//! Also home of the restart replay driver (design D5): the same machine,
//! driven synchronously before the first session is opened.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use tokio::sync::{mpsc, watch};
use tracing::{debug, info, warn};

use faultforge_fault::digest::Digest;
use faultforge_fault::params::validate_params;
use faultforge_fault::proto::to_wire_i32;
use faultforge_fault::protocol::{
    EventBody, InstanceId, Level, Phase, PluginCommand, PluginEvent, PluginInput,
};
use faultforge_fault::state::InstanceState;
use faultforge_proto::unix_ms;
use faultforge_proto::v1::{
    AgentMessage, FaultEvent, InstanceReport, InstanceStatus, RunFault, TaintStatus, agent_message,
};

use crate::catalog::{VerifiedPlugin, resolve_verified};
use crate::journal::{JOURNAL_VERSION, Journal, JournalEntry};
use crate::machine::{self, Effect, Event, Outcome, State, StopCause, StopClock};
use crate::runner::{self, InvocationOutcome, StdoutLine};
use crate::taint::{Taint, TaintRecord};

// ===== Connectivity signal (published by the session layer, design D4/D8) =====

/// Whether the agent currently holds a registered session with the master.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnState {
    /// Registered and pumping frames.
    Connected,
    /// No session; `since_unix_ms` is when contact was lost (preserved across
    /// failed reconnect attempts so the self-abort threshold keeps counting).
    Lost {
        /// When master contact was lost, unix milliseconds.
        since_unix_ms: i64,
    },
}

// ===== Shared runtime context =====

/// Everything an instance task needs, shared by all instances.
#[derive(Debug)]
pub struct RuntimeCtx {
    /// Catalog root.
    pub plugin_root: PathBuf,
    /// The instance journal.
    pub journal: Journal,
    /// The host taint record.
    pub taint: Taint,
    /// Outbound frames to the master (buffered across reconnects).
    pub out: mpsc::Sender<AgentMessage>,
    /// Connectivity, for the master-loss timer input.
    pub conn: watch::Receiver<ConnState>,
    /// Master-loss self-abort threshold.
    pub loss_threshold: Duration,
    /// Hard cap per plugin invocation.
    pub invocation_timeout: Duration,
}

// ===== Frame constructors =====

fn status_frame(
    instance_id: &str,
    state: InstanceState,
    reason: Option<String>,
    plugin_digest: &str,
    ts_unix_ms: i64,
) -> AgentMessage {
    AgentMessage {
        payload: Some(agent_message::Payload::InstanceStatus(InstanceStatus {
            instance_id: instance_id.to_string(),
            state: to_wire_i32(state),
            ts_unix_ms,
            reason: reason.unwrap_or_default(),
            plugin_digest: plugin_digest.to_string(),
        })),
    }
}

fn fault_event_frame(instance_id: &str, ndjson_line: String) -> AgentMessage {
    AgentMessage {
        payload: Some(agent_message::Payload::FaultEvent(FaultEvent {
            instance_id: instance_id.to_string(),
            ndjson_line,
        })),
    }
}

/// The `TaintStatus` frame for the current host taint state (both values are
/// information — `tainted: false` tells the master the host is usable).
#[must_use]
pub fn taint_status_frame(current: Option<&TaintRecord>, now_ms: i64) -> AgentMessage {
    let status = current.map_or(
        TaintStatus {
            tainted: false,
            reason: String::new(),
            ts_unix_ms: now_ms,
        },
        |record| TaintStatus {
            tainted: true,
            reason: record.reason.clone(),
            ts_unix_ms: record.ts_unix_ms,
        },
    );
    AgentMessage {
        payload: Some(agent_message::Payload::TaintStatus(status)),
    }
}

/// An agent-authored NDJSON `log` line (stderr and malformed-output telemetry),
/// wrapped exactly like a plugin event so operators see one stream shape.
fn agent_log_frame(instance_id: &InstanceId, now: SystemTime, msg: String) -> Option<AgentMessage> {
    let ts = humantime::format_rfc3339_seconds(now).to_string();
    let event = PluginEvent::log(ts, instance_id.clone(), Level::Error, msg);
    match serde_json::to_string(&event) {
        Ok(line) => Some(fault_event_frame(instance_id.as_str(), line)),
        Err(e) => {
            warn!(error = %e, "could not encode agent log event; dropping");
            None
        }
    }
}

// ===== Supervisor =====

/// Live view of one instance, updated by its task, read by the report builder.
#[derive(Debug, Clone)]
struct InstanceView {
    state: InstanceState,
    plugin_digest: String,
}

struct InstanceHandle {
    view: Arc<Mutex<InstanceView>>,
    stop_tx: mpsc::UnboundedSender<StopCause>,
}

/// Owns all fault instances: spawns on `RunFault`, signals on `AbortFault`,
/// snapshots for `InstanceReport`. Terminal handles are retained so a duplicate
/// `RunFault` is recognized for the agent's lifetime (ADR-0002 §12).
pub struct Supervisor {
    ctx: Arc<RuntimeCtx>,
    instances: HashMap<InstanceId, InstanceHandle>,
}

/// Accept exactly the ids that are safe as journal file stems: non-empty, no
/// path separators, no `.`/`..`. The master mints ids, but the agent must not
/// trust the wire with its filesystem.
fn valid_instance_id(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

impl Supervisor {
    /// A supervisor over `ctx` with no instances.
    #[must_use]
    pub fn new(ctx: Arc<RuntimeCtx>) -> Self {
        Self {
            ctx,
            instances: HashMap::new(),
        }
    }

    /// Handle a `RunFault` frame: spawn a new instance, or drop a duplicate —
    /// inject is never re-executed for a known id (ADR-0002 §12).
    pub fn handle_run_fault(&mut self, run: RunFault) {
        if !valid_instance_id(&run.instance_id) {
            warn!(instance_id = %run.instance_id, "dropping RunFault with unusable instance id");
            return;
        }
        let id = InstanceId::from(run.instance_id.as_str());
        if self.instances.contains_key(&id) {
            info!(instance_id = %id, "duplicate RunFault dropped; instance already known");
            return;
        }
        let view = Arc::new(Mutex::new(InstanceView {
            state: InstanceState::Pending,
            plugin_digest: run.plugin_digest.clone(),
        }));
        let (stop_tx, stop_rx) = mpsc::unbounded_channel();
        tokio::spawn(run_instance(
            Arc::clone(&self.ctx),
            run,
            Arc::clone(&view),
            stop_rx,
        ));
        self.instances.insert(id, InstanceHandle { view, stop_tx });
    }

    /// Handle an `AbortFault` frame.
    pub fn handle_abort_fault(&mut self, instance_id: &str) {
        let id = InstanceId::from(instance_id);
        if let Some(handle) = self.instances.get(&id) {
            // A send error means the task already finished — terminal states
            // absorb aborts anyway.
            let _ = handle.stop_tx.send(StopCause::OperatorAbort);
        } else {
            warn!(instance_id, "AbortFault for unknown instance; ignoring");
        }
    }

    /// Handle a `ClearTaint` frame: remove the taint record (idempotent — an
    /// untainted host is already clear) and report the resulting state. A
    /// failed removal keeps the quarantine and says why.
    pub fn handle_clear_taint(&self) {
        let taint = self.ctx.taint.clone();
        let out = self.ctx.out.clone();
        tokio::spawn(async move {
            let cleared = {
                let taint = taint.clone();
                tokio::task::spawn_blocking(move || taint.clear()).await
            };
            let now_ms = unix_ms(SystemTime::now());
            let frame = match cleared {
                // Re-read instead of assuming clean: if a record somehow still
                // exists the quarantine must fail closed.
                Ok(Ok(())) => {
                    info!("taint cleared by operator command");
                    taint_status_frame(taint.current().as_ref(), now_ms)
                }
                Ok(Err(e)) => {
                    warn!(error = %e, "could not remove taint record; host stays tainted");
                    taint_status_frame(
                        Some(&TaintRecord {
                            reason: format!("taint clear failed: {e}"),
                            ts_unix_ms: now_ms,
                            instance_id: String::new(),
                        }),
                        now_ms,
                    )
                }
                Err(join_err) => {
                    warn!(error = %join_err, "taint clear task failed; host stays tainted");
                    taint_status_frame(
                        Some(&TaintRecord {
                            reason: format!("taint clear task failed: {join_err}"),
                            ts_unix_ms: now_ms,
                            instance_id: String::new(),
                        }),
                        now_ms,
                    )
                }
            };
            if out.send(frame).await.is_err() {
                debug!("outbound channel closed while reporting taint clear");
            }
        });
    }

    /// The reconciliation snapshot sent after every registration: all live
    /// (non-terminal) instances with their states and digests. Empty is
    /// meaningful ("nothing running") and is still sent.
    #[must_use]
    pub fn report_frame(&self, now_ms: i64) -> AgentMessage {
        let mut statuses: Vec<InstanceStatus> = self
            .instances
            .iter()
            .filter_map(|(id, handle)| {
                let view = handle
                    .view
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                if matches!(
                    view.state,
                    InstanceState::Done | InstanceState::Aborted | InstanceState::Error
                ) {
                    None
                } else {
                    Some(InstanceStatus {
                        instance_id: id.to_string(),
                        state: to_wire_i32(view.state),
                        ts_unix_ms: now_ms,
                        reason: String::new(),
                        plugin_digest: view.plugin_digest,
                    })
                }
            })
            .collect();
        statuses.sort_by(|a, b| a.instance_id.cmp(&b.instance_id));
        AgentMessage {
            payload: Some(agent_message::Payload::InstanceReport(InstanceReport {
                statuses,
            })),
        }
    }
}

// ===== The per-instance task =====

/// Setup checks (taint, digest reference, catalog + digest, params, duration).
/// Pure over its inputs apart from filesystem reads; returns what the instance
/// task needs to invoke the plugin.
fn setup(
    plugin_root: &Path,
    taint: &Taint,
    run: &RunFault,
) -> Result<(VerifiedPlugin, serde_json::Map<String, serde_json::Value>), String> {
    if let Some(record) = taint.current() {
        return Err(format!("host tainted: {}", record.reason));
    }
    let expected =
        Digest::parse(&run.plugin_digest).map_err(|e| format!("invalid plugin digest: {e}"))?;
    let plugin = resolve_verified(
        plugin_root,
        &run.plugin_name,
        &run.plugin_version,
        &expected,
    )
    .map_err(|e| e.to_string())?;
    let params: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&run.params_json)
        .map_err(|e| format!("params_json is not a JSON object: {e}"))?;
    validate_params(&plugin.manifest.params_schema, &params)
        .map_err(|e| format!("invalid params: {e}"))?;
    if run.duration_secs > plugin.manifest.max_duration_secs {
        return Err(format!(
            "duration {}s exceeds plugin max_duration_secs {}s",
            run.duration_secs, plugin.manifest.max_duration_secs
        ));
    }
    Ok((plugin, params))
}

async fn send_out(ctx: &RuntimeCtx, msg: AgentMessage) {
    if ctx.out.send(msg).await.is_err() {
        debug!("outbound channel closed; agent is shutting down");
    }
}

/// Forward one invocation's telemetry: well-formed lines verbatim (the plugin
/// spec requires content-unmodified forwarding), malformed lines and stderr as
/// agent-authored error logs. Plugin `status` lines never reach the machine —
/// two-track telemetry by construction.
async fn forward_telemetry(ctx: &RuntimeCtx, id: &InstanceId, invocation: &runner::Invocation) {
    for line in &invocation.lines {
        match line {
            StdoutLine::Event { raw, event } => {
                if let EventBody::Status { state } = &event.body
                    && !state.plugin_emittable()
                {
                    warn!(instance_id = %id, state = ?state,
                        "plugin asserted an agent-owned state; forwarding as telemetry only");
                }
                send_out(ctx, fault_event_frame(id.as_str(), raw.clone())).await;
            }
            StdoutLine::Malformed { raw } => {
                let msg = format!("malformed plugin output: {raw}");
                if let Some(frame) = agent_log_frame(id, SystemTime::now(), msg) {
                    send_out(ctx, frame).await;
                }
            }
        }
    }
    for line in invocation.stderr.lines().filter(|l| !l.trim().is_empty()) {
        if let Some(frame) = agent_log_frame(id, SystemTime::now(), format!("stderr: {line}")) {
            send_out(ctx, frame).await;
        }
    }
}

fn invocation_event(
    command: PluginCommand,
    outcome: &InvocationOutcome,
    timeout: Duration,
) -> Event {
    match outcome {
        InvocationOutcome::Exited(disposition) => Event::CommandFinished {
            command,
            disposition: *disposition,
        },
        InvocationOutcome::TimedOut => Event::InvocationFailed {
            command,
            reason: format!("killed after invocation timeout ({timeout:?})"),
        },
        InvocationOutcome::Signalled => Event::InvocationFailed {
            command,
            reason: "terminated by signal".to_string(),
        },
        InvocationOutcome::Failed(reason) => Event::InvocationFailed {
            command,
            reason: reason.clone(),
        },
    }
}

/// Everything a single instance's shell needs, fixed at spawn time.
struct InstanceEnv {
    ctx: Arc<RuntimeCtx>,
    id: InstanceId,
    run: RunFault,
    params: serde_json::Map<String, serde_json::Value>,
    /// Absolute dead-man deadline handed to every plugin invocation.
    deadline_unix: i64,
    start_ms: i64,
}

impl InstanceEnv {
    async fn emit_status(&self, state: InstanceState, reason: Option<String>) {
        send_out(
            &self.ctx,
            status_frame(
                self.id.as_str(),
                state,
                reason,
                &self.run.plugin_digest,
                unix_ms(SystemTime::now()),
            ),
        )
        .await;
    }

    /// Run one digest-gated plugin invocation off the async runtime, feeding
    /// any stop requests that arrive meanwhile to the front of the event queue
    /// (so the machine records them before acting on the invocation result).
    async fn invoke_gated(
        &self,
        command: PluginCommand,
        phase: Phase,
        stop_rx: &mut mpsc::UnboundedReceiver<StopCause>,
        queue: &mut std::collections::VecDeque<Event>,
    ) -> Event {
        // Re-verify the digest before every invocation (design D7): trust the
        // disk only for the exact bytes being executed right now.
        let root = self.ctx.plugin_root.clone();
        let (name, version, digest_str) = (
            self.run.plugin_name.clone(),
            self.run.plugin_version.clone(),
            self.run.plugin_digest.clone(),
        );
        let resolved = tokio::task::spawn_blocking(move || {
            let expected =
                Digest::parse(&digest_str).map_err(|e| format!("invalid plugin digest: {e}"))?;
            resolve_verified(&root, &name, &version, &expected).map_err(|e| e.to_string())
        })
        .await;
        let plugin = match resolved {
            Ok(Ok(plugin)) => plugin,
            Ok(Err(reason)) => return Event::InvokeRejected { command, reason },
            Err(join_err) => {
                return Event::InvocationFailed {
                    command,
                    reason: format!("digest verification task failed: {join_err}"),
                };
            }
        };

        let input = PluginInput {
            instance_id: self.id.clone(),
            params: self.params.clone(),
            deadline_unix: self.deadline_unix,
            phase,
        };
        let timeout = self.ctx.invocation_timeout;
        let mut handle = tokio::task::spawn_blocking(move || {
            runner::invoke(&plugin.entrypoint, command, &input, timeout)
        });
        let joined = loop {
            tokio::select! {
                joined = &mut handle => break joined,
                Some(cause) = stop_rx.recv() => {
                    queue.push_front(Event::StopRequested { cause });
                }
            }
        };
        match joined {
            Ok(invocation) => {
                forward_telemetry(&self.ctx, &self.id, &invocation).await;
                invocation_event(command, &invocation.outcome, timeout)
            }
            Err(join_err) => Event::InvocationFailed {
                command,
                reason: format!("invocation task failed: {join_err}"),
            },
        }
    }

    fn journal_entry(&self) -> JournalEntry {
        JournalEntry {
            version: JOURNAL_VERSION,
            instance_id: self.id.to_string(),
            plugin_name: self.run.plugin_name.clone(),
            plugin_version: self.run.plugin_version.clone(),
            plugin_digest: self.run.plugin_digest.clone(),
            params: self.params.clone(),
            started_unix_ms: self.start_ms,
            duration_secs: self.run.duration_secs,
            grace_secs: self.run.grace_secs,
            deadline_unix: self.deadline_unix,
        }
    }

    /// Perform one machine effect, queueing any result event.
    async fn apply_effect(
        &self,
        effect: Effect,
        stop_rx: &mut mpsc::UnboundedReceiver<StopCause>,
        queue: &mut std::collections::VecDeque<Event>,
    ) {
        match effect {
            Effect::EmitStatus { state, reason } => self.emit_status(state, reason).await,
            Effect::Invoke { command, phase } => {
                let event = self.invoke_gated(command, phase, stop_rx, queue).await;
                queue.push_back(event);
            }
            Effect::WriteJournal => {
                let event = match self.ctx.journal.write(&self.journal_entry()) {
                    Ok(()) => Event::JournalWritten,
                    Err(e) => Event::JournalWriteFailed {
                        reason: e.to_string(),
                    },
                };
                queue.push_back(event);
            }
            Effect::RemoveJournal => {
                if let Err(e) = self.ctx.journal.remove(self.id.as_str()) {
                    // Worst case a stale entry causes one spurious, idempotent
                    // cleanup at next startup.
                    warn!(instance_id = %self.id, error = %e, "could not remove journal entry");
                }
            }
            Effect::MarkTainted { reason } => {
                mark_tainted(&self.ctx.taint, self.id.as_str(), &reason, &self.ctx.out).await;
            }
        }
    }
}

/// Wait in `ACTIVE` for the earliest stop: duration end, master-loss threshold,
/// dead-man, or an operator abort — re-arming whenever connectivity changes.
async fn wait_for_stop(
    ctx: &RuntimeCtx,
    duration_end_ms: i64,
    deadman_ms: i64,
    stop_rx: &mut mpsc::UnboundedReceiver<StopCause>,
) -> Event {
    let mut conn = ctx.conn.clone();
    #[allow(clippy::cast_possible_truncation)]
    let loss_threshold_ms = ctx.loss_threshold.as_millis() as i64;
    loop {
        let now_ms = unix_ms(SystemTime::now());
        let lost_since = match *conn.borrow_and_update() {
            ConnState::Connected => None,
            ConnState::Lost { since_unix_ms } => Some(since_unix_ms),
        };
        let (at_ms, cause) = machine::next_stop(&StopClock {
            duration_end_ms,
            deadman_ms,
            master_lost_since_ms: lost_since,
            loss_threshold_ms,
        });
        if at_ms <= now_ms {
            return Event::StopRequested { cause };
        }
        let sleep_for = Duration::from_millis(u64::try_from(at_ms - now_ms).unwrap_or(0));
        tokio::select! {
            () = tokio::time::sleep(sleep_for) => return Event::StopRequested { cause },
            changed = conn.changed() => {
                if changed.is_err() {
                    // Session layer gone (shutdown): no more connectivity
                    // updates will arrive; sleep out the current deadline.
                    tokio::time::sleep(sleep_for).await;
                    return Event::StopRequested { cause };
                }
            }
            Some(cause) = stop_rx.recv() => return Event::StopRequested { cause },
        }
    }
}

/// The imperative shell of one instance: performs effects, turns results into
/// events, and lets [`machine::step`] decide everything.
async fn run_instance(
    ctx: Arc<RuntimeCtx>,
    run: RunFault,
    view: Arc<Mutex<InstanceView>>,
    mut stop_rx: mpsc::UnboundedReceiver<StopCause>,
) {
    let start_ms = unix_ms(SystemTime::now());
    let deadline_unix = start_ms / 1000 + i64::from(run.duration_secs) + i64::from(run.grace_secs);
    let env = InstanceEnv {
        id: InstanceId::from(run.instance_id.as_str()),
        ctx,
        params: serde_json::Map::new(),
        deadline_unix,
        start_ms,
        run,
    };
    let deadman_ms = deadline_unix * 1000;

    env.emit_status(InstanceState::Pending, None).await;

    let setup_result = {
        let (root, taint, run_clone) = (
            env.ctx.plugin_root.clone(),
            env.ctx.taint.clone(),
            env.run.clone(),
        );
        tokio::task::spawn_blocking(move || setup(&root, &taint, &run_clone)).await
    };
    let (env, initial_event) = match setup_result {
        Ok(Ok((_plugin, params))) => (InstanceEnv { params, ..env }, Event::SetupOk),
        Ok(Err(reason)) => {
            // The reason also rides the Error `InstanceStatus` to the master, but a
            // local log makes a rejected RunFault visible on the host even with the
            // master link down — notably a SEC-2 version rejection before any join.
            warn!(instance_id = %env.id, reason = %reason, "setup rejected RunFault");
            (env, Event::SetupFailed { reason })
        }
        Err(join_err) => (
            env,
            Event::SetupFailed {
                reason: format!("setup task failed: {join_err}"),
            },
        ),
    };

    let mut state = State::Pending;
    let mut queue = std::collections::VecDeque::from([initial_event]);
    let mut active_since_ms: Option<i64> = None;

    loop {
        let event = if let Some(event) = queue.pop_front() {
            event
        } else if state.is_terminal() {
            break;
        } else if state == State::Active {
            let duration_end_ms =
                active_since_ms.unwrap_or(env.start_ms) + i64::from(env.run.duration_secs) * 1000;
            wait_for_stop(&env.ctx, duration_end_ms, deadman_ms, &mut stop_rx).await
        } else {
            // Every non-active, non-terminal state was entered with a pending
            // effect whose result event is queued; reaching here is a machine/
            // shell mismatch. Fail safe: treat as a dead-man stop.
            warn!(instance_id = %env.id, state = ?state, "instance wedged without a pending event");
            Event::StopRequested {
                cause: StopCause::Deadman,
            }
        };

        let (next_state, effects) = machine::step(state, event);
        state = next_state;
        {
            let mut guard = view
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.state = state.instance_state();
        }
        if state == State::Active && active_since_ms.is_none() {
            active_since_ms = Some(unix_ms(SystemTime::now()));
        }

        for effect in effects {
            env.apply_effect(effect, &mut stop_rx, &mut queue).await;
        }
    }
    info!(instance_id = %env.id, state = ?state.instance_state(), "instance finished");
}

async fn mark_tainted(
    taint: &Taint,
    instance_id: &str,
    reason: &str,
    out: &mpsc::Sender<AgentMessage>,
) {
    let now_ms = unix_ms(SystemTime::now());
    let record = TaintRecord {
        reason: reason.to_string(),
        ts_unix_ms: now_ms,
        instance_id: instance_id.to_string(),
    };
    if let Err(e) = taint.mark(&record) {
        // The quarantine could not be persisted — the loudest failure the agent
        // has. It still reports the taint for this process's lifetime.
        warn!(error = %e, "FAILED TO PERSIST TAINT RECORD");
    }
    let frame = taint_status_frame(taint.current().as_ref().or(Some(&record)), now_ms);
    if out.send(frame).await.is_err() {
        debug!("outbound channel closed while reporting taint");
    }
}

// ===== Restart replay (design D5) =====

/// Recover every journaled instance before the first session: `abort` then
/// `cleanup`, digest-gated, ending `ABORTED` (before the dead-man) or `ERROR`
/// (past it). Returns the frames to send after the first registration, in
/// order. Runs synchronously — call from `spawn_blocking`.
#[must_use]
pub fn replay_journal(
    plugin_root: &Path,
    journal: &Journal,
    taint: &Taint,
    invocation_timeout: Duration,
    now: SystemTime,
) -> Vec<AgentMessage> {
    let entries = match journal.list() {
        Ok(entries) => entries,
        Err(e) => {
            warn!(error = %e, "could not list instance journal; skipping replay");
            return vec![];
        }
    };
    let mut frames = vec![];
    for entry in entries {
        match entry {
            Ok(entry) => frames.extend(replay_entry(
                plugin_root,
                journal,
                taint,
                &entry,
                invocation_timeout,
                now,
            )),
            Err(e) => {
                // An unreadable journal entry means an unknown fault may be
                // live and cannot be recovered — quarantine, fail closed.
                warn!(error = %e, "unreadable journal entry; tainting host");
                let record = TaintRecord {
                    reason: format!("unreadable journal entry at startup: {e}"),
                    ts_unix_ms: unix_ms(now),
                    instance_id: String::new(),
                };
                if let Err(mark_err) = taint.mark(&record) {
                    warn!(error = %mark_err, "FAILED TO PERSIST TAINT RECORD");
                }
                frames.push(taint_status_frame(
                    taint.current().as_ref().or(Some(&record)),
                    unix_ms(now),
                ));
            }
        }
    }
    frames
}

fn replay_entry(
    plugin_root: &Path,
    journal: &Journal,
    taint: &Taint,
    entry: &JournalEntry,
    invocation_timeout: Duration,
    now: SystemTime,
) -> Vec<AgentMessage> {
    let id = InstanceId::from(entry.instance_id.as_str());
    let now_ms = unix_ms(now);
    // Past the dead-man the safety promise was already broken: ERROR, not ABORTED.
    let outcome = if now_ms / 1000 < entry.deadline_unix {
        Outcome::Aborted
    } else {
        Outcome::Error
    };
    let mut frames = vec![];
    let (mut state, initial_effects) =
        State::replay_recovery(outcome, "recovered after agent restart");
    let mut queue: std::collections::VecDeque<Event> = std::collections::VecDeque::new();
    let mut effects: std::collections::VecDeque<Effect> = initial_effects.into();

    loop {
        while let Some(effect) = effects.pop_front() {
            match effect {
                Effect::EmitStatus {
                    state: reported,
                    reason,
                } => frames.push(status_frame(
                    entry.instance_id.as_str(),
                    reported,
                    reason,
                    &entry.plugin_digest,
                    unix_ms(SystemTime::now()),
                )),
                Effect::Invoke { command, phase } => {
                    let (event, telemetry) =
                        replay_invoke(plugin_root, entry, &id, command, phase, invocation_timeout);
                    frames.extend(telemetry);
                    queue.push_back(event);
                }
                Effect::RemoveJournal => {
                    if let Err(e) = journal.remove(entry.instance_id.as_str()) {
                        warn!(instance_id = %entry.instance_id, error = %e,
                            "could not remove journal entry after replay");
                    }
                }
                Effect::MarkTainted { reason } => {
                    let record = TaintRecord {
                        reason: reason.clone(),
                        ts_unix_ms: unix_ms(SystemTime::now()),
                        instance_id: entry.instance_id.clone(),
                    };
                    if let Err(e) = taint.mark(&record) {
                        warn!(error = %e, "FAILED TO PERSIST TAINT RECORD");
                    }
                    frames.push(taint_status_frame(
                        taint.current().as_ref().or(Some(&record)),
                        unix_ms(SystemTime::now()),
                    ));
                }
                // Replay never writes journals — the entry already exists.
                Effect::WriteJournal => {}
            }
        }
        let Some(event) = queue.pop_front() else {
            break;
        };
        let (next_state, new_effects) = machine::step(state, event);
        state = next_state;
        effects.extend(new_effects);
        if state.is_terminal() && queue.is_empty() && effects.is_empty() {
            break;
        }
    }
    frames
}

/// One digest-gated sync invocation during replay, with its telemetry frames.
fn replay_invoke(
    plugin_root: &Path,
    entry: &JournalEntry,
    id: &InstanceId,
    command: PluginCommand,
    phase: Phase,
    invocation_timeout: Duration,
) -> (Event, Vec<AgentMessage>) {
    let expected = match Digest::parse(&entry.plugin_digest) {
        Ok(digest) => digest,
        Err(e) => {
            return (
                Event::InvokeRejected {
                    command,
                    reason: format!("journaled digest invalid: {e}"),
                },
                vec![],
            );
        }
    };
    let plugin = match resolve_verified(
        plugin_root,
        &entry.plugin_name,
        &entry.plugin_version,
        &expected,
    ) {
        Ok(plugin) => plugin,
        Err(e) => {
            return (
                Event::InvokeRejected {
                    command,
                    reason: e.to_string(),
                },
                vec![],
            );
        }
    };
    let input = PluginInput {
        instance_id: id.clone(),
        params: entry.params.clone(),
        deadline_unix: entry.deadline_unix,
        phase,
    };
    let invocation = runner::invoke(&plugin.entrypoint, command, &input, invocation_timeout);
    let mut frames = vec![];
    for line in &invocation.lines {
        match line {
            StdoutLine::Event { raw, .. } => {
                frames.push(fault_event_frame(id.as_str(), raw.clone()));
            }
            StdoutLine::Malformed { raw } => {
                if let Some(frame) = agent_log_frame(
                    id,
                    SystemTime::now(),
                    format!("malformed plugin output: {raw}"),
                ) {
                    frames.push(frame);
                }
            }
        }
    }
    for line in invocation.stderr.lines().filter(|l| !l.trim().is_empty()) {
        if let Some(frame) = agent_log_frame(id, SystemTime::now(), format!("stderr: {line}")) {
            frames.push(frame);
        }
    }
    (
        invocation_event(command, &invocation.outcome, invocation_timeout),
        frames,
    )
}

// ===== Unit tests =====

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_id_boundary_accepts_sane_ids() {
        for ok in ["exp1-web01-0", "a", "A.B:c_d-9"] {
            assert!(valid_instance_id(ok), "{ok} should be accepted");
        }
    }

    #[test]
    fn instance_id_boundary_rejects_path_hazards() {
        for bad in ["", ".", "..", "a/b", "../x", "a b", "id\n"] {
            assert!(!valid_instance_id(bad), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn taint_frame_reports_both_values() {
        let clean = taint_status_frame(None, 5);
        match clean.payload {
            Some(agent_message::Payload::TaintStatus(ts)) => {
                assert!(!ts.tainted);
                assert_eq!(ts.ts_unix_ms, 5);
            }
            other => panic!("expected TaintStatus, got {other:?}"),
        }
        let record = TaintRecord {
            reason: "r".into(),
            ts_unix_ms: 3,
            instance_id: "i".into(),
        };
        let tainted = taint_status_frame(Some(&record), 5);
        match tainted.payload {
            Some(agent_message::Payload::TaintStatus(ts)) => {
                assert!(ts.tainted);
                assert_eq!(ts.reason, "r");
                assert_eq!(ts.ts_unix_ms, 3);
            }
            other => panic!("expected TaintStatus, got {other:?}"),
        }
    }
}
