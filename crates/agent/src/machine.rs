//! The pure per-instance state machine (functional core, design D2).
//!
//! Every ordering rule of ADR-0002 lives here as a transition:
//! `step(state, event) -> (state, effects)`. The imperative shell
//! ([`crate::supervisor`]) performs the effects, turns their results into the
//! next events, and never decides state itself. Nothing in this module does IO,
//! reads clocks, or awaits.

use faultforge_fault::protocol::{Disposition, Phase, PluginCommand};
use faultforge_fault::state::InstanceState;

// ===== Stop causes and outcomes =====

/// Why an instance is being stopped — the three timer causes plus the operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopCause {
    /// The experiment `duration` elapsed: the normal, graceful end.
    DurationElapsed,
    /// The master sent `AbortFault`.
    OperatorAbort,
    /// The master was unreachable beyond the configured threshold (ADR-0002 §5).
    MasterLost,
    /// The dead-man deadline (`start + duration + grace`) was reached.
    Deadman,
}

impl StopCause {
    /// The human-readable reason carried in the resulting `InstanceStatus`.
    #[must_use]
    pub fn reason(self) -> &'static str {
        match self {
            Self::DurationElapsed => "duration elapsed",
            Self::OperatorAbort => "operator abort",
            Self::MasterLost => "master unreachable beyond threshold; self-abort",
            Self::Deadman => "dead-man deadline reached",
        }
    }

    /// The terminal outcome recovery aims for when stopped by this cause.
    fn outcome(self) -> Outcome {
        match self {
            Self::DurationElapsed => Outcome::Done,
            Self::OperatorAbort | Self::MasterLost => Outcome::Aborted,
            Self::Deadman => Outcome::Error,
        }
    }

    /// The recovery command sequence for this cause. A graceful stop is an
    /// orderly `cleanup`; every other stop is "stop now, save the host":
    /// `abort` first, then the idempotent `cleanup` (ADR-0002 §7).
    fn recovery_steps(self) -> (PluginCommand, Vec<PluginCommand>) {
        match self {
            Self::DurationElapsed => (PluginCommand::Cleanup, vec![]),
            Self::OperatorAbort | Self::MasterLost | Self::Deadman => {
                (PluginCommand::Abort, vec![PluginCommand::Cleanup])
            }
        }
    }
}

/// The terminal state recovery is heading toward.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Clean completion.
    Done,
    /// Stopped early with the host restored.
    Aborted,
    /// A malfunction; dominates the other outcomes.
    Error,
}

impl Outcome {
    fn terminal_state(self) -> InstanceState {
        match self {
            Self::Done => InstanceState::Done,
            Self::Aborted => InstanceState::Aborted,
            Self::Error => InstanceState::Error,
        }
    }
}

// ===== States =====

/// Recovery bookkeeping: which command is running, what is still queued, and
/// what terminal state a clean recovery yields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recovery {
    current: PluginCommand,
    remaining: Vec<PluginCommand>,
    outcome: Outcome,
    /// One idempotent retry of the current command is allowed before the host
    /// is tainted (ADR-0002 §9).
    retried: bool,
    reason: Option<String>,
}

/// The machine's state. Richer than the wire [`InstanceState`]: pre-inject
/// phases record a pending stop request so an abort that arrives mid-invocation
/// is honoured at the next step without ever injecting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// Known, nothing run yet; the shell is performing setup checks.
    Pending,
    /// The install-phase `preflight` invocation is running.
    PreflightInstall {
        /// A stop that arrived mid-invocation, honoured before proceeding.
        stop: Option<StopCause>,
    },
    /// The runtime-phase `preflight` invocation is running.
    PreflightRuntime {
        /// A stop that arrived mid-invocation, honoured before proceeding.
        stop: Option<StopCause>,
    },
    /// Preflight passed; the journal write is in flight (before `inject`, D5).
    WritingJournal {
        /// A stop that arrived mid-write, honoured before injecting.
        stop: Option<StopCause>,
    },
    /// The `inject` invocation is running.
    Injecting {
        /// A stop that arrived mid-inject; honoured by recovering right after.
        stop: Option<StopCause>,
    },
    /// The fault is live; the shell is waiting on the safety timer.
    Active,
    /// Recovery commands are running.
    Recovering(Recovery),
    /// Terminal: completed cleanly.
    Done,
    /// Terminal: stopped with the host restored (or never touched).
    Aborted,
    /// Terminal: malfunction (possibly tainted host).
    Error,
}

impl State {
    /// The wire-visible lifecycle state for this machine state.
    #[must_use]
    pub fn instance_state(&self) -> InstanceState {
        match self {
            Self::Pending => InstanceState::Pending,
            Self::PreflightInstall { .. }
            | Self::PreflightRuntime { .. }
            | Self::WritingJournal { .. } => InstanceState::Preflight,
            Self::Injecting { .. } => InstanceState::Injecting,
            Self::Active => InstanceState::Active,
            Self::Recovering(_) => InstanceState::Recovering,
            Self::Done => InstanceState::Done,
            Self::Aborted => InstanceState::Aborted,
            Self::Error => InstanceState::Error,
        }
    }

    /// `true` once the instance can never change state again.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Done | Self::Aborted | Self::Error)
    }

    /// Entry point for restart replay (design D5): a journaled instance is
    /// ambiguous by definition, so it goes straight to abort+cleanup recovery.
    /// Returns the initial state plus the effects announcing it.
    #[must_use]
    pub fn replay_recovery(outcome: Outcome, reason: &str) -> (Self, Vec<Effect>) {
        let state = Self::Recovering(Recovery {
            current: PluginCommand::Abort,
            remaining: vec![PluginCommand::Cleanup],
            outcome,
            retried: false,
            reason: Some(reason.to_string()),
        });
        let effects = vec![
            Effect::EmitStatus {
                state: InstanceState::Recovering,
                reason: Some(reason.to_string()),
            },
            Effect::Invoke {
                command: PluginCommand::Abort,
                phase: Phase::Runtime,
            },
        ];
        (state, effects)
    }
}

// ===== Events and effects =====

/// What happened — produced by the shell from effect results, timers, and frames.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Setup (taint check, catalog resolve + digest, params, duration) passed.
    SetupOk,
    /// Setup failed; the host is untouched.
    SetupFailed {
        /// Which check failed.
        reason: String,
    },
    /// The journal entry is durably on disk.
    JournalWritten,
    /// The journal entry could not be written; injecting would strand the fault.
    JournalWriteFailed {
        /// The IO failure.
        reason: String,
    },
    /// A plugin invocation ran to completion and exited.
    CommandFinished {
        /// The command that ran.
        command: PluginCommand,
        /// Its classified exit code.
        disposition: Disposition,
    },
    /// A plugin invocation started but did not complete (timeout, signal, spawn
    /// failure after the process began) — the host state is unknown.
    InvocationFailed {
        /// The command that failed.
        command: PluginCommand,
        /// What went wrong.
        reason: String,
    },
    /// The invocation was refused before the process ever started (digest
    /// mismatch on re-verification, unresolvable plugin) — the host is untouched
    /// by this command.
    InvokeRejected {
        /// The command that was refused.
        command: PluginCommand,
        /// Why it was refused.
        reason: String,
    },
    /// A stop was requested (operator frame or a safety-timer cause).
    StopRequested {
        /// Why.
        cause: StopCause,
    },
}

/// What the shell must do — the machine's only way to act on the world.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Send an agent-authoritative `InstanceStatus` transition.
    EmitStatus {
        /// The state to report.
        state: InstanceState,
        /// Optional human-readable reason.
        reason: Option<String>,
    },
    /// Run one plugin command (digest-gated by the shell before spawning).
    Invoke {
        /// The lifecycle command.
        command: PluginCommand,
        /// `Install` only for the first preflight; `Runtime` otherwise.
        phase: Phase,
    },
    /// Persist the instance journal entry (before `inject`, design D5).
    WriteJournal,
    /// Remove the instance journal entry (terminal state reached).
    RemoveJournal,
    /// Quarantine the host (ADR-0002 §9) and report `TaintStatus`.
    MarkTainted {
        /// Why recovery failed.
        reason: String,
    },
}

// ===== Transition function =====

fn aborted_with(reason: String) -> (State, Vec<Effect>) {
    (
        State::Aborted,
        vec![Effect::EmitStatus {
            state: InstanceState::Aborted,
            reason: Some(reason),
        }],
    )
}

fn start_recovery(cause: StopCause, extra_reason: Option<String>) -> (State, Vec<Effect>) {
    let (current, remaining) = cause.recovery_steps();
    let reason = extra_reason.unwrap_or_else(|| cause.reason().to_string());
    let state = State::Recovering(Recovery {
        current,
        remaining,
        outcome: cause.outcome(),
        retried: false,
        reason: Some(reason.clone()),
    });
    let effects = vec![
        Effect::EmitStatus {
            state: InstanceState::Recovering,
            reason: Some(reason),
        },
        Effect::Invoke {
            command: current,
            phase: Phase::Runtime,
        },
    ];
    (state, effects)
}

/// `disposition` rendered for a status reason.
fn describe(disposition: Disposition) -> String {
    match disposition {
        Disposition::Success => "success".to_string(),
        Disposition::PreflightFailed => "preflight precondition failed".to_string(),
        Disposition::InjectFailed => "inject failed".to_string(),
        Disposition::RecoveryFailed => "cleanup/abort failed".to_string(),
        Disposition::Unexpected(code) => format!("unexpected exit code {code}"),
    }
}

fn step_preflight(which: Phase, stop: Option<StopCause>, event: Event) -> (State, Vec<Effect>) {
    let rebuild = |stop| match which {
        Phase::Install => State::PreflightInstall { stop },
        Phase::Runtime => State::PreflightRuntime { stop },
    };
    match event {
        Event::StopRequested { cause } => (rebuild(stop.or(Some(cause))), vec![]),
        Event::CommandFinished {
            command: PluginCommand::Preflight,
            disposition: Disposition::Success,
        } => {
            if let Some(cause) = stop {
                return aborted_with(format!("{}; never injected", cause.reason()));
            }
            match which {
                Phase::Install => (
                    State::PreflightRuntime { stop: None },
                    vec![Effect::Invoke {
                        command: PluginCommand::Preflight,
                        phase: Phase::Runtime,
                    }],
                ),
                Phase::Runtime => (
                    State::WritingJournal { stop: None },
                    vec![Effect::WriteJournal],
                ),
            }
        }
        Event::CommandFinished {
            command: PluginCommand::Preflight,
            disposition,
        } => {
            let phase_name = match which {
                Phase::Install => "install",
                Phase::Runtime => "runtime",
            };
            aborted_with(format!("{phase_name} preflight: {}", describe(disposition)))
        }
        Event::InvocationFailed { reason, .. } | Event::InvokeRejected { reason, .. } => {
            aborted_with(format!("preflight invocation failed: {reason}"))
        }
        // Out-of-band events — a stray non-`Preflight` command, setup, or
        // journal — cannot advance preflight: a confused shell must never
        // crash an instance (issue #24). Absorb them, preserving `stop`.
        _ => (rebuild(stop), vec![]),
    }
}

fn step_recovering(mut rec: Recovery, event: Event) -> (State, Vec<Effect>) {
    match event {
        // Dead-man during recovery: the outcome escalates to ERROR but the
        // recovery sequence keeps running — stopping it would strand the host.
        Event::StopRequested {
            cause: StopCause::Deadman,
        } => {
            rec.outcome = Outcome::Error;
            (State::Recovering(rec), vec![])
        }
        Event::CommandFinished {
            command,
            disposition: Disposition::Success,
        } if command == rec.current => {
            if rec.remaining.is_empty() {
                let terminal = rec.outcome.terminal_state();
                (
                    match rec.outcome {
                        Outcome::Done => State::Done,
                        Outcome::Aborted => State::Aborted,
                        Outcome::Error => State::Error,
                    },
                    vec![
                        Effect::RemoveJournal,
                        Effect::EmitStatus {
                            state: terminal,
                            reason: rec.reason,
                        },
                    ],
                )
            } else {
                let next = rec.remaining.remove(0);
                rec.current = next;
                rec.retried = false;
                (
                    State::Recovering(rec),
                    vec![Effect::Invoke {
                        command: next,
                        phase: Phase::Runtime,
                    }],
                )
            }
        }
        Event::CommandFinished {
            command,
            disposition,
        } if command == rec.current => recovery_failure(rec, &describe(disposition)),
        Event::InvocationFailed { command, reason } | Event::InvokeRejected { command, reason }
            if command == rec.current =>
        {
            recovery_failure(rec, &reason)
        }
        _ => (State::Recovering(rec), vec![]),
    }
}

/// A recovery command failed: retry the idempotent operation once; if it fails
/// again, quarantine the host (ADR-0002 §9) and end in `ERROR`.
fn recovery_failure(mut rec: Recovery, detail: &str) -> (State, Vec<Effect>) {
    if rec.retried {
        let reason = format!("recovery failed after retry ({}): {detail}", rec.current);
        (
            State::Error,
            vec![
                Effect::MarkTainted {
                    reason: reason.clone(),
                },
                Effect::RemoveJournal,
                Effect::EmitStatus {
                    state: InstanceState::Error,
                    reason: Some(reason),
                },
            ],
        )
    } else {
        rec.retried = true;
        let command = rec.current;
        (
            State::Recovering(rec),
            vec![Effect::Invoke {
                command,
                phase: Phase::Runtime,
            }],
        )
    }
}

fn step_pending(event: Event) -> (State, Vec<Effect>) {
    match event {
        Event::SetupOk => (
            State::PreflightInstall { stop: None },
            vec![
                Effect::EmitStatus {
                    state: InstanceState::Preflight,
                    reason: None,
                },
                Effect::Invoke {
                    command: PluginCommand::Preflight,
                    phase: Phase::Install,
                },
            ],
        ),
        Event::SetupFailed { reason } => aborted_with(reason),
        Event::StopRequested { cause } => {
            aborted_with(format!("{}; never started", cause.reason()))
        }
        _ => (State::Pending, vec![]),
    }
}

fn step_writing_journal(stop: Option<StopCause>, event: Event) -> (State, Vec<Effect>) {
    match event {
        Event::StopRequested { cause } => (
            State::WritingJournal {
                stop: stop.or(Some(cause)),
            },
            vec![],
        ),
        Event::JournalWritten => {
            if let Some(cause) = stop {
                (
                    State::Aborted,
                    vec![
                        Effect::RemoveJournal,
                        Effect::EmitStatus {
                            state: InstanceState::Aborted,
                            reason: Some(format!("{}; never injected", cause.reason())),
                        },
                    ],
                )
            } else {
                (
                    State::Injecting { stop: None },
                    vec![
                        Effect::EmitStatus {
                            state: InstanceState::Injecting,
                            reason: None,
                        },
                        Effect::Invoke {
                            command: PluginCommand::Inject,
                            phase: Phase::Runtime,
                        },
                    ],
                )
            }
        }
        Event::JournalWriteFailed { reason } => {
            // Injecting without a journal could strand a fault with no
            // record — the exact failure the journal exists to prevent.
            aborted_with(format!(
                "journal write failed; refusing to inject: {reason}"
            ))
        }
        _ => (State::WritingJournal { stop }, vec![]),
    }
}

fn step_injecting(stop: Option<StopCause>, event: Event) -> (State, Vec<Effect>) {
    match event {
        Event::StopRequested { cause } => (
            State::Injecting {
                stop: stop.or(Some(cause)),
            },
            vec![],
        ),
        Event::CommandFinished {
            command: PluginCommand::Inject,
            disposition: Disposition::Success,
        } => {
            if let Some(cause) = stop {
                start_recovery(cause, None)
            } else {
                (
                    State::Active,
                    vec![Effect::EmitStatus {
                        state: InstanceState::Active,
                        reason: None,
                    }],
                )
            }
        }
        Event::CommandFinished {
            command: PluginCommand::Inject,
            disposition: Disposition::InjectFailed,
        } => (
            // Exit 20 asserts the host is unaffected — no recovery needed.
            State::Aborted,
            vec![
                Effect::RemoveJournal,
                Effect::EmitStatus {
                    state: InstanceState::Aborted,
                    reason: Some("inject failed; host unaffected".to_string()),
                },
            ],
        ),
        Event::CommandFinished {
            command: PluginCommand::Inject,
            disposition,
        } => {
            // An unexpected inject exit leaves the host state unknown —
            // ambiguous means abort (ADR-0002 §12), ending in ERROR.
            start_recovery(
                StopCause::Deadman,
                Some(format!("inject: {}", describe(disposition))),
            )
        }
        Event::InvocationFailed { reason, .. } => start_recovery(
            StopCause::Deadman,
            Some(format!("inject invocation failed: {reason}")),
        ),
        Event::InvokeRejected { reason, .. } => (
            // Refused before the process started: the host is untouched.
            State::Aborted,
            vec![
                Effect::RemoveJournal,
                Effect::EmitStatus {
                    state: InstanceState::Aborted,
                    reason: Some(format!("inject refused: {reason}")),
                },
            ],
        ),
        _ => (State::Injecting { stop }, vec![]),
    }
}

/// Advance the machine by one event.
///
/// Total over all `(state, event)` pairs: impossible combinations are ignored
/// (state returned unchanged, no effects) rather than panicking, so a confused
/// shell can never crash an instance out of supervision.
#[must_use]
pub fn step(state: State, event: Event) -> (State, Vec<Effect>) {
    match state {
        State::Pending => step_pending(event),
        State::PreflightInstall { stop } => step_preflight(Phase::Install, stop, event),
        State::PreflightRuntime { stop } => step_preflight(Phase::Runtime, stop, event),
        State::WritingJournal { stop } => step_writing_journal(stop, event),
        State::Injecting { stop } => step_injecting(stop, event),
        State::Active => match event {
            Event::StopRequested { cause } => start_recovery(cause, None),
            _ => (State::Active, vec![]),
        },
        State::Recovering(rec) => step_recovering(rec, event),
        terminal @ (State::Done | State::Aborted | State::Error) => (terminal, vec![]),
    }
}

// ===== The single safety deadline (design D4) =====

/// Inputs to the one-timer model: three absolute candidate deadlines, of which
/// the earliest fires. All times are unix milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StopClock {
    /// `active_since + duration`: the graceful stop.
    pub duration_end_ms: i64,
    /// `start + duration + grace`: the dead-man backstop.
    pub deadman_ms: i64,
    /// When master contact was lost, if it currently is.
    pub master_lost_since_ms: Option<i64>,
    /// The configured self-abort threshold.
    pub loss_threshold_ms: i64,
}

/// The earliest pending stop and its cause — "the deadline or the silence,
/// whichever comes first" (ADR-0002 §5) as one `min`.
///
/// Ties prefer the gentlest outcome: `DurationElapsed` over `MasterLost` over
/// `Deadman`.
#[must_use]
pub fn next_stop(clock: &StopClock) -> (i64, StopCause) {
    let mut best = (clock.duration_end_ms, StopCause::DurationElapsed);
    if let Some(since) = clock.master_lost_since_ms {
        let at = since.saturating_add(clock.loss_threshold_ms);
        if at < best.0 {
            best = (at, StopCause::MasterLost);
        }
    }
    if clock.deadman_ms < best.0 {
        best = (clock.deadman_ms, StopCause::Deadman);
    }
    best
}

// ===== Unit tests =====

#[cfg(test)]
mod tests {
    use super::*;

    fn finished(command: PluginCommand, disposition: Disposition) -> Event {
        Event::CommandFinished {
            command,
            disposition,
        }
    }

    /// Drive a fresh machine through the given events, collecting all effects.
    fn drive(events: Vec<Event>) -> (State, Vec<Effect>) {
        let mut state = State::Pending;
        let mut all = Vec::new();
        for event in events {
            let (next, effects) = step(state, event);
            state = next;
            all.extend(effects);
        }
        (state, all)
    }

    fn emitted_states(effects: &[Effect]) -> Vec<InstanceState> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::EmitStatus { state, .. } => Some(*state),
                _ => None,
            })
            .collect()
    }

    fn happy_path_events() -> Vec<Event> {
        vec![
            Event::SetupOk,
            finished(PluginCommand::Preflight, Disposition::Success),
            finished(PluginCommand::Preflight, Disposition::Success),
            Event::JournalWritten,
            finished(PluginCommand::Inject, Disposition::Success),
            Event::StopRequested {
                cause: StopCause::DurationElapsed,
            },
            finished(PluginCommand::Cleanup, Disposition::Success),
        ]
    }

    #[test]
    fn happy_path_reaches_done_and_emits_every_transition() {
        let (state, effects) = drive(happy_path_events());
        assert_eq!(state, State::Done);
        assert_eq!(
            emitted_states(&effects),
            vec![
                InstanceState::Preflight,
                InstanceState::Injecting,
                InstanceState::Active,
                InstanceState::Recovering,
                InstanceState::Done,
            ]
        );
        // Graceful stop runs cleanup only, never abort.
        let invoked: Vec<_> = effects
            .iter()
            .filter_map(|e| match e {
                Effect::Invoke { command, phase } => Some((*command, *phase)),
                _ => None,
            })
            .collect();
        assert_eq!(
            invoked,
            vec![
                (PluginCommand::Preflight, Phase::Install),
                (PluginCommand::Preflight, Phase::Runtime),
                (PluginCommand::Inject, Phase::Runtime),
                (PluginCommand::Cleanup, Phase::Runtime),
            ]
        );
    }

    #[test]
    fn journal_is_written_before_inject_and_removed_at_terminal() {
        let (_, effects) = drive(happy_path_events());
        let positions: Vec<_> = effects
            .iter()
            .enumerate()
            .filter_map(|(i, e)| match e {
                Effect::WriteJournal => Some(("write", i)),
                Effect::RemoveJournal => Some(("remove", i)),
                Effect::Invoke {
                    command: PluginCommand::Inject,
                    ..
                } => Some(("inject", i)),
                _ => None,
            })
            .collect();
        assert_eq!(positions.len(), 3);
        assert_eq!(positions[0].0, "write");
        assert_eq!(positions[1].0, "inject");
        assert_eq!(positions[2].0, "remove");
    }

    #[test]
    fn setup_failure_aborts_without_any_invocation() {
        let (state, effects) = drive(vec![Event::SetupFailed {
            reason: "plugin not in catalog".to_string(),
        }]);
        assert_eq!(state, State::Aborted);
        assert!(
            !effects.iter().any(|e| matches!(e, Effect::Invoke { .. })),
            "no plugin command may run after setup failure"
        );
        assert_eq!(emitted_states(&effects), vec![InstanceState::Aborted]);
    }

    #[test]
    fn preflight_exit_10_aborts_without_inject() {
        let (state, effects) = drive(vec![
            Event::SetupOk,
            finished(PluginCommand::Preflight, Disposition::Success),
            finished(PluginCommand::Preflight, Disposition::PreflightFailed),
        ]);
        assert_eq!(state, State::Aborted);
        assert!(!effects.iter().any(|e| matches!(
            e,
            Effect::Invoke {
                command: PluginCommand::Inject,
                ..
            }
        )));
    }

    #[test]
    fn preflight_unexpected_exit_aborts() {
        let (state, _) = drive(vec![
            Event::SetupOk,
            finished(PluginCommand::Preflight, Disposition::Unexpected(1)),
        ]);
        assert_eq!(state, State::Aborted);
    }

    #[test]
    fn preflight_ignores_out_of_band_command() {
        // A confused shell must never crash an instance: a finished command
        // other than `Preflight` during preflight is ignored, not acted on.
        for phase in [Phase::Install, Phase::Runtime] {
            let (mut events, expected_state) = match phase {
                Phase::Install => (vec![Event::SetupOk], State::PreflightInstall { stop: None }),
                Phase::Runtime => (
                    vec![
                        Event::SetupOk,
                        finished(PluginCommand::Preflight, Disposition::Success),
                    ],
                    State::PreflightRuntime { stop: None },
                ),
            };
            let (before_state, before_effects) = drive(events.clone());
            assert_eq!(before_state, expected_state);

            events.push(finished(PluginCommand::Cleanup, Disposition::Success));
            let (after_state, after_effects) = drive(events);
            assert_eq!(
                after_state, expected_state,
                "stray command must leave {phase:?} preflight unchanged"
            );
            assert_eq!(
                after_effects, before_effects,
                "stray command must emit no new effects in {phase:?} preflight"
            );
        }
    }

    #[test]
    fn inject_exit_20_aborts_without_recovery() {
        let (state, effects) = drive(vec![
            Event::SetupOk,
            finished(PluginCommand::Preflight, Disposition::Success),
            finished(PluginCommand::Preflight, Disposition::Success),
            Event::JournalWritten,
            finished(PluginCommand::Inject, Disposition::InjectFailed),
        ]);
        assert_eq!(state, State::Aborted);
        // Host unaffected by contract: no abort/cleanup invoked, journal removed.
        assert!(!effects.iter().any(|e| matches!(
            e,
            Effect::Invoke {
                command: PluginCommand::Abort | PluginCommand::Cleanup,
                ..
            }
        )));
        assert!(effects.iter().any(|e| matches!(e, Effect::RemoveJournal)));
    }

    #[test]
    fn inject_unexpected_exit_recovers_to_error() {
        let (state, effects) = drive(vec![
            Event::SetupOk,
            finished(PluginCommand::Preflight, Disposition::Success),
            finished(PluginCommand::Preflight, Disposition::Success),
            Event::JournalWritten,
            finished(PluginCommand::Inject, Disposition::Unexpected(1)),
            finished(PluginCommand::Abort, Disposition::Success),
            finished(PluginCommand::Cleanup, Disposition::Success),
        ]);
        assert_eq!(state, State::Error);
        assert!(effects.iter().any(|e| matches!(
            e,
            Effect::Invoke {
                command: PluginCommand::Abort,
                ..
            }
        )));
    }

    #[test]
    fn operator_abort_from_active_runs_abort_then_cleanup() {
        let (state, effects) = drive(vec![
            Event::SetupOk,
            finished(PluginCommand::Preflight, Disposition::Success),
            finished(PluginCommand::Preflight, Disposition::Success),
            Event::JournalWritten,
            finished(PluginCommand::Inject, Disposition::Success),
            Event::StopRequested {
                cause: StopCause::OperatorAbort,
            },
            finished(PluginCommand::Abort, Disposition::Success),
            finished(PluginCommand::Cleanup, Disposition::Success),
        ]);
        assert_eq!(state, State::Aborted);
        let invoked: Vec<_> = effects
            .iter()
            .filter_map(|e| match e {
                Effect::Invoke { command, .. } => Some(*command),
                _ => None,
            })
            .collect();
        assert_eq!(
            &invoked[2..],
            &[
                PluginCommand::Inject,
                PluginCommand::Abort,
                PluginCommand::Cleanup
            ]
        );
    }

    #[test]
    fn master_loss_self_aborts() {
        let (state, _) = drive(vec![
            Event::SetupOk,
            finished(PluginCommand::Preflight, Disposition::Success),
            finished(PluginCommand::Preflight, Disposition::Success),
            Event::JournalWritten,
            finished(PluginCommand::Inject, Disposition::Success),
            Event::StopRequested {
                cause: StopCause::MasterLost,
            },
            finished(PluginCommand::Abort, Disposition::Success),
            finished(PluginCommand::Cleanup, Disposition::Success),
        ]);
        assert_eq!(state, State::Aborted);
    }

    #[test]
    fn deadman_forces_recovery_and_ends_error() {
        let (state, _) = drive(vec![
            Event::SetupOk,
            finished(PluginCommand::Preflight, Disposition::Success),
            finished(PluginCommand::Preflight, Disposition::Success),
            Event::JournalWritten,
            finished(PluginCommand::Inject, Disposition::Success),
            Event::StopRequested {
                cause: StopCause::Deadman,
            },
            finished(PluginCommand::Abort, Disposition::Success),
            finished(PluginCommand::Cleanup, Disposition::Success),
        ]);
        assert_eq!(state, State::Error);
    }

    #[test]
    fn deadman_during_recovery_escalates_outcome_to_error() {
        let (state, _) = drive(vec![
            Event::SetupOk,
            finished(PluginCommand::Preflight, Disposition::Success),
            finished(PluginCommand::Preflight, Disposition::Success),
            Event::JournalWritten,
            finished(PluginCommand::Inject, Disposition::Success),
            Event::StopRequested {
                cause: StopCause::OperatorAbort,
            },
            finished(PluginCommand::Abort, Disposition::Success),
            Event::StopRequested {
                cause: StopCause::Deadman,
            },
            finished(PluginCommand::Cleanup, Disposition::Success),
        ]);
        // Would have been ABORTED; the dead-man passing during recovery makes it ERROR.
        assert_eq!(state, State::Error);
    }

    #[test]
    fn recovery_failure_retries_once_then_taints() {
        let (state, effects) = drive(vec![
            Event::SetupOk,
            finished(PluginCommand::Preflight, Disposition::Success),
            finished(PluginCommand::Preflight, Disposition::Success),
            Event::JournalWritten,
            finished(PluginCommand::Inject, Disposition::Success),
            Event::StopRequested {
                cause: StopCause::DurationElapsed,
            },
            finished(PluginCommand::Cleanup, Disposition::RecoveryFailed),
            finished(PluginCommand::Cleanup, Disposition::RecoveryFailed),
        ]);
        assert_eq!(state, State::Error);
        let cleanup_invocations = effects
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    Effect::Invoke {
                        command: PluginCommand::Cleanup,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(cleanup_invocations, 2, "exactly one retry");
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::MarkTainted { .. })),
            "second failure must taint the host"
        );
    }

    #[test]
    fn recovery_failure_then_retry_success_does_not_taint() {
        let (state, effects) = drive(vec![
            Event::SetupOk,
            finished(PluginCommand::Preflight, Disposition::Success),
            finished(PluginCommand::Preflight, Disposition::Success),
            Event::JournalWritten,
            finished(PluginCommand::Inject, Disposition::Success),
            Event::StopRequested {
                cause: StopCause::DurationElapsed,
            },
            finished(PluginCommand::Cleanup, Disposition::RecoveryFailed),
            finished(PluginCommand::Cleanup, Disposition::Success),
        ]);
        assert_eq!(state, State::Done);
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::MarkTainted { .. }))
        );
    }

    #[test]
    fn abort_during_install_preflight_is_honoured_before_runtime_preflight() {
        let (state, effects) = drive(vec![
            Event::SetupOk,
            Event::StopRequested {
                cause: StopCause::OperatorAbort,
            },
            finished(PluginCommand::Preflight, Disposition::Success),
        ]);
        assert_eq!(state, State::Aborted);
        // Only the install preflight ran; nothing else was invoked.
        let invocations = effects
            .iter()
            .filter(|e| matches!(e, Effect::Invoke { .. }))
            .count();
        assert_eq!(invocations, 1);
    }

    #[test]
    fn abort_during_journal_write_removes_journal_and_never_injects() {
        let (state, effects) = drive(vec![
            Event::SetupOk,
            finished(PluginCommand::Preflight, Disposition::Success),
            finished(PluginCommand::Preflight, Disposition::Success),
            Event::StopRequested {
                cause: StopCause::OperatorAbort,
            },
            Event::JournalWritten,
        ]);
        assert_eq!(state, State::Aborted);
        assert!(effects.iter().any(|e| matches!(e, Effect::RemoveJournal)));
        assert!(!effects.iter().any(|e| matches!(
            e,
            Effect::Invoke {
                command: PluginCommand::Inject,
                ..
            }
        )));
    }

    #[test]
    fn abort_during_inject_recovers_after_inject_completes() {
        let (state, effects) = drive(vec![
            Event::SetupOk,
            finished(PluginCommand::Preflight, Disposition::Success),
            finished(PluginCommand::Preflight, Disposition::Success),
            Event::JournalWritten,
            Event::StopRequested {
                cause: StopCause::OperatorAbort,
            },
            finished(PluginCommand::Inject, Disposition::Success),
            finished(PluginCommand::Abort, Disposition::Success),
            finished(PluginCommand::Cleanup, Disposition::Success),
        ]);
        assert_eq!(state, State::Aborted);
        assert!(effects.iter().any(|e| matches!(
            e,
            Effect::Invoke {
                command: PluginCommand::Abort,
                ..
            }
        )));
    }

    #[test]
    fn journal_write_failure_refuses_to_inject() {
        let (state, effects) = drive(vec![
            Event::SetupOk,
            finished(PluginCommand::Preflight, Disposition::Success),
            finished(PluginCommand::Preflight, Disposition::Success),
            Event::JournalWriteFailed {
                reason: "disk full".to_string(),
            },
        ]);
        assert_eq!(state, State::Aborted);
        assert!(!effects.iter().any(|e| matches!(
            e,
            Effect::Invoke {
                command: PluginCommand::Inject,
                ..
            }
        )));
    }

    #[test]
    fn inject_rejected_by_digest_gate_aborts_without_recovery() {
        let (state, effects) = drive(vec![
            Event::SetupOk,
            finished(PluginCommand::Preflight, Disposition::Success),
            finished(PluginCommand::Preflight, Disposition::Success),
            Event::JournalWritten,
            Event::InvokeRejected {
                command: PluginCommand::Inject,
                reason: "digest mismatch".to_string(),
            },
        ]);
        assert_eq!(state, State::Aborted);
        assert!(!effects.iter().any(|e| matches!(
            e,
            Effect::Invoke {
                command: PluginCommand::Abort | PluginCommand::Cleanup,
                ..
            }
        )));
    }

    #[test]
    fn terminal_states_absorb_every_event() {
        for terminal in [State::Done, State::Aborted, State::Error] {
            for event in [
                Event::SetupOk,
                Event::StopRequested {
                    cause: StopCause::Deadman,
                },
                finished(PluginCommand::Cleanup, Disposition::Success),
            ] {
                let (state, effects) = step(terminal.clone(), event);
                assert_eq!(state, terminal);
                assert!(effects.is_empty());
            }
        }
    }

    #[test]
    fn plugin_cannot_drive_state_there_is_no_plugin_status_event() {
        // The two-track rule is structural: the machine has no event carrying a
        // plugin-asserted state, so a plugin NDJSON line cannot reach it. This
        // test documents the invariant by checking the shell-facing surface.
        let (state, effects) = step(State::Active, Event::SetupOk);
        assert_eq!(state, State::Active);
        assert!(effects.is_empty());
    }

    #[test]
    fn replay_recovery_runs_abort_then_cleanup() {
        let (state, effects) = State::replay_recovery(Outcome::Aborted, "agent restarted");
        assert_eq!(state.instance_state(), InstanceState::Recovering);
        assert!(matches!(
            effects[1],
            Effect::Invoke {
                command: PluginCommand::Abort,
                ..
            }
        ));
        let (state, _) = step(state, finished(PluginCommand::Abort, Disposition::Success));
        let (state, effects) = step(
            state,
            finished(PluginCommand::Cleanup, Disposition::Success),
        );
        assert_eq!(state, State::Aborted);
        assert!(effects.iter().any(|e| matches!(e, Effect::RemoveJournal)));
    }

    // ----- the single safety deadline -----

    #[test]
    fn duration_end_is_the_default_stop() {
        let clock = StopClock {
            duration_end_ms: 10_000,
            deadman_ms: 15_000,
            master_lost_since_ms: None,
            loss_threshold_ms: 30_000,
        };
        assert_eq!(next_stop(&clock), (10_000, StopCause::DurationElapsed));
    }

    #[test]
    fn master_loss_fires_before_duration_when_earlier() {
        let clock = StopClock {
            duration_end_ms: 10_000,
            deadman_ms: 15_000,
            master_lost_since_ms: Some(2_000),
            loss_threshold_ms: 3_000,
        };
        assert_eq!(next_stop(&clock), (5_000, StopCause::MasterLost));
    }

    #[test]
    fn reconnect_rearms_back_to_duration() {
        // Same clock as above but the master came back: the loss candidate is gone.
        let clock = StopClock {
            duration_end_ms: 10_000,
            deadman_ms: 15_000,
            master_lost_since_ms: None,
            loss_threshold_ms: 3_000,
        };
        assert_eq!(next_stop(&clock), (10_000, StopCause::DurationElapsed));
    }

    #[test]
    fn deadman_wins_when_earliest() {
        // Inject took so long that the dead-man now precedes the duration end.
        let clock = StopClock {
            duration_end_ms: 20_000,
            deadman_ms: 15_000,
            master_lost_since_ms: None,
            loss_threshold_ms: 30_000,
        };
        assert_eq!(next_stop(&clock), (15_000, StopCause::Deadman));
    }

    #[test]
    fn ties_prefer_the_gentlest_cause() {
        let clock = StopClock {
            duration_end_ms: 10_000,
            deadman_ms: 10_000,
            master_lost_since_ms: Some(7_000),
            loss_threshold_ms: 3_000,
        };
        assert_eq!(next_stop(&clock), (10_000, StopCause::DurationElapsed));
    }
}
