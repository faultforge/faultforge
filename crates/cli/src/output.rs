//! Pure output rendering: converts agent data to formatted strings.
//!
//! All functions are pure — they take already-fetched data plus `now` and
//! return a `String`. No I/O is performed here.

use std::time::SystemTime;

use crate::api::Agent;
use crate::api::model::{Experiment, ExperimentSummary};
use crate::cli::OutputFormat;

/// Render a list of agents in the chosen format.
///
/// `now` is used to compute relative ages for table output; it is ignored for JSON.
///
/// # Errors
///
/// Returns an error if JSON serialisation fails (should be infallible for well-typed data).
pub fn render_agents(agents: &[Agent], format: &OutputFormat, now: SystemTime) -> String {
    match format {
        OutputFormat::Json => render_agents_json(agents),
        OutputFormat::Table => render_agents_table(agents, now),
    }
}

/// Render a single agent in the chosen format.
///
/// # Errors
///
/// Returns an error if JSON serialisation fails.
pub fn render_agent(agent: &Agent, format: &OutputFormat, now: SystemTime) -> String {
    match format {
        OutputFormat::Json => render_agent_json(agent),
        OutputFormat::Table => render_agent_table(agent, now),
    }
}

/// Return the age of `last_seen` relative to `now` as a human string (e.g. "5s ago").
///
/// Returns `"unknown"` if `now` is before `last_seen` (clock skew or fresh registration).
pub fn relative_age(now: SystemTime, last_seen: SystemTime) -> String {
    match now.duration_since(last_seen) {
        Ok(d) => {
            let secs = d.as_secs();
            if secs < 60 {
                format!("{secs}s ago")
            } else if secs < 3600 {
                format!("{}m ago", secs / 60)
            } else {
                format!("{}h ago", secs / 3600)
            }
        }
        Err(_) => "just now".to_string(),
    }
}

fn render_agents_json(agents: &[Agent]) -> String {
    serde_json::to_string_pretty(agents).unwrap_or_else(|e| format!("[render error: {e}]"))
}

fn render_agent_json(agent: &Agent) -> String {
    serde_json::to_string_pretty(agent).unwrap_or_else(|e| format!("{{render error: {e}}}"))
}

/// The table cell for a host quarantine flag.
fn taint_cell(tainted: bool) -> &'static str {
    if tainted { "TAINTED" } else { "-" }
}

fn render_agents_table(agents: &[Agent], now: SystemTime) -> String {
    if agents.is_empty() {
        return "(no agents registered)".to_string();
    }
    let header = format!(
        "{:<20} {:<20} {:<10} {}",
        "HOSTNAME", "NAME", "TAINT", "LAST SEEN"
    );
    let sep = "-".repeat(header.len());
    let rows: Vec<String> = agents
        .iter()
        .map(|a| {
            format!(
                "{:<20} {:<20} {:<10} {}",
                a.hostname,
                a.name,
                taint_cell(a.tainted),
                relative_age(now, a.last_seen)
            )
        })
        .collect();
    std::iter::once(header)
        .chain(std::iter::once(sep))
        .chain(rows)
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_agent_table(agent: &Agent, now: SystemTime) -> String {
    format!(
        "hostname:  {}\nname:      {}\ntainted:   {}\nlast seen: {}",
        agent.hostname,
        agent.name,
        agent.tainted,
        relative_age(now, agent.last_seen)
    )
}

// ===== Experiments =====

/// Render an experiment list in the chosen format.
pub fn render_experiments(
    experiments: &[ExperimentSummary],
    format: &OutputFormat,
    now: SystemTime,
) -> String {
    match format {
        OutputFormat::Json => serde_json::to_string_pretty(experiments)
            .unwrap_or_else(|e| format!("[render error: {e}]")),
        OutputFormat::Table => render_experiments_table(experiments, now),
    }
}

/// Render one experiment (full record) in the chosen format.
pub fn render_experiment(
    experiment: &Experiment,
    format: &OutputFormat,
    now: SystemTime,
) -> String {
    match format {
        OutputFormat::Json => serde_json::to_string_pretty(experiment)
            .unwrap_or_else(|e| format!("{{render error: {e}}}")),
        OutputFormat::Table => render_experiment_table(experiment, now),
    }
}

fn ms_to_time(ms: i64) -> SystemTime {
    u64::try_from(ms).map_or(std::time::UNIX_EPOCH, |ms| {
        std::time::UNIX_EPOCH + std::time::Duration::from_millis(ms)
    })
}

fn render_experiments_table(experiments: &[ExperimentSummary], now: SystemTime) -> String {
    if experiments.is_empty() {
        return "(no experiments)".to_string();
    }
    let header = format!(
        "{:<28} {:<16} {:<10} {:<10} {}",
        "ID", "NAME", "STATE", "INSTANCES", "STARTED"
    );
    let sep = "-".repeat(header.len());
    let rows: Vec<String> = experiments
        .iter()
        .map(|e| {
            format!(
                "{:<28} {:<16} {:<10} {:<10} {}",
                e.id,
                e.name,
                e.state,
                e.instances,
                relative_age(now, ms_to_time(e.started_unix_ms))
            )
        })
        .collect();
    std::iter::once(header)
        .chain(std::iter::once(sep))
        .chain(rows)
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_experiment_table(experiment: &Experiment, now: SystemTime) -> String {
    let mut lines = vec![
        format!("id:       {}", experiment.id),
        format!("name:     {}", experiment.name),
        format!("state:    {}", experiment.state),
        format!("outcome:  {}", experiment.outcome.as_deref().unwrap_or("-")),
        format!("grace:    {}s", experiment.grace_secs),
        format!(
            "started:  {}",
            relative_age(now, ms_to_time(experiment.started_unix_ms))
        ),
    ];
    if let Some(cause) = &experiment.cause {
        lines.push(format!(
            "cause:    {} (host: {}, instance: {})",
            cause.reason,
            cause.hostname.as_deref().unwrap_or("-"),
            cause.instance_id.as_deref().unwrap_or("-"),
        ));
    }
    lines.push(String::new());
    let header = format!(
        "{:<40} {:<20} {:<12} {}",
        "INSTANCE", "HOSTNAME", "STATE", "REASON"
    );
    lines.push(header.clone());
    lines.push("-".repeat(header.len()));
    for instance in &experiment.instances {
        lines.push(format!(
            "{:<40} {:<20} {:<12} {}",
            instance.instance_id, instance.hostname, instance.state, instance.reason
        ));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    fn agent(hostname: &str, ms: u64) -> Agent {
        Agent {
            hostname: hostname.to_string(),
            name: hostname.to_string(),
            last_seen: UNIX_EPOCH + Duration::from_millis(ms),
            tainted: false,
        }
    }

    fn now_at(ms: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_millis(ms)
    }

    // --- relative_age tests ---

    #[test]
    fn relative_age_seconds() {
        let now = now_at(10_000);
        let last = now_at(5_000);
        assert_eq!(relative_age(now, last), "5s ago");
    }

    #[test]
    fn relative_age_minutes() {
        let now = now_at(120_000);
        let last = now_at(0);
        assert_eq!(relative_age(now, last), "2m ago");
    }

    #[test]
    fn relative_age_hours() {
        let now = now_at(7_200_000);
        let last = now_at(0);
        assert_eq!(relative_age(now, last), "2h ago");
    }

    #[test]
    fn relative_age_future_returns_just_now() {
        let now = now_at(1_000);
        let last = now_at(5_000);
        assert_eq!(relative_age(now, last), "just now");
    }

    // --- JSON rendering ---

    #[test]
    fn render_agents_json_empty() {
        let result = render_agents(&[], &OutputFormat::Json, now_at(0));
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&result).unwrap();
        assert!(parsed.is_empty());
    }

    #[test]
    fn render_agents_json_single() {
        let agents = vec![agent("web-01", 1000)];
        let result = render_agents(&agents, &OutputFormat::Json, now_at(5000));
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed[0]["hostname"], "web-01");
        assert_eq!(parsed[0]["last_seen_unix_ms"], 1000);
    }

    #[test]
    fn render_agents_json_multi() {
        let agents = vec![agent("a", 0), agent("b", 1000)];
        let result = render_agents(&agents, &OutputFormat::Json, now_at(2000));
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed.as_array().unwrap().len(), 2);
    }

    // --- Table rendering ---

    #[test]
    fn render_agents_table_empty() {
        let result = render_agents(&[], &OutputFormat::Table, now_at(0));
        assert!(result.contains("no agents"));
    }

    #[test]
    fn render_agents_table_single() {
        let agents = vec![agent("web-01", 0)];
        let result = render_agents(&agents, &OutputFormat::Table, now_at(5000));
        assert!(result.contains("web-01"));
        assert!(result.contains("5s ago"));
    }

    #[test]
    fn render_agent_table_single() {
        let a = agent("db-01", 0);
        let result = render_agent(&a, &OutputFormat::Table, now_at(10_000));
        assert!(result.contains("db-01"));
        assert!(result.contains("10s ago"));
    }

    #[test]
    fn render_agents_table_marks_taint() {
        let mut tainted = agent("web-01", 0);
        tainted.tainted = true;
        let result = render_agents(&[tainted], &OutputFormat::Table, now_at(1_000));
        assert!(result.contains("TAINTED"));
    }

    // --- Experiment rendering ---

    fn experiment() -> Experiment {
        use crate::api::model::{Cause, Instance};
        Experiment {
            id: "exp-1000-1".to_string(),
            name: "it".to_string(),
            state: "ERROR".to_string(),
            outcome: Some("ERROR".to_string()),
            grace_secs: 10,
            started_unix_ms: 0,
            deadline_unix_ms: 45_000,
            cause: Some(Cause {
                hostname: Some("web-01".to_string()),
                instance_id: Some("exp-1000-1:web-01:0".to_string()),
                reason: "instance ERROR: boom".to_string(),
                ts_unix_ms: 2_000,
            }),
            instances: vec![Instance {
                instance_id: "exp-1000-1:web-01:0".to_string(),
                hostname: "web-01".to_string(),
                action_index: 0,
                state: "ERROR".to_string(),
                reason: "boom".to_string(),
                updated_unix_ms: 2_000,
            }],
        }
    }

    #[test]
    fn render_experiment_json_round_trips() {
        let result = render_experiment(&experiment(), &OutputFormat::Json, now_at(5_000));
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["id"], "exp-1000-1");
        assert_eq!(parsed["outcome"], "ERROR");
        assert_eq!(parsed["instances"][0]["state"], "ERROR");
    }

    #[test]
    fn render_experiment_table_names_the_cause_and_instances() {
        let result = render_experiment(&experiment(), &OutputFormat::Table, now_at(5_000));
        assert!(result.contains("state:    ERROR"));
        assert!(result.contains("instance ERROR: boom"));
        assert!(result.contains("exp-1000-1:web-01:0"));
    }

    #[test]
    fn render_experiments_table_lists_rows() {
        let summary = ExperimentSummary {
            id: "exp-1000-1".to_string(),
            name: "it".to_string(),
            state: "RUNNING".to_string(),
            outcome: None,
            started_unix_ms: 0,
            instances: 3,
        };
        let result = render_experiments(&[summary], &OutputFormat::Table, now_at(1_000));
        assert!(result.contains("exp-1000-1"));
        assert!(result.contains("RUNNING"));
        let empty = render_experiments(&[], &OutputFormat::Table, now_at(0));
        assert!(empty.contains("no experiments"));
    }
}
