//! Pure output rendering: converts agent data to formatted strings.
//!
//! All functions are pure — they take already-fetched data plus `now` and
//! return a `String`. No I/O is performed here.

use std::time::SystemTime;

use crate::api::Agent;
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

fn render_agents_table(agents: &[Agent], now: SystemTime) -> String {
    if agents.is_empty() {
        return "(no agents registered)".to_string();
    }
    let header = format!("{:<20} {:<20} {}", "HOSTNAME", "NAME", "LAST SEEN");
    let sep = "-".repeat(header.len());
    let rows: Vec<String> = agents
        .iter()
        .map(|a| {
            format!(
                "{:<20} {:<20} {}",
                a.hostname,
                a.name,
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
        "hostname:  {}\nname:      {}\nlast seen: {}",
        agent.hostname,
        agent.name,
        relative_age(now, agent.last_seen)
    )
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
}
