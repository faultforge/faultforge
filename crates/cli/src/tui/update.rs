//! Pure state machine: `update(Model, Msg, now) -> (Model, Option<Effect>)`.
//!
//! No I/O, no async — all decision logic lives here and is unit-testable
//! with a fixed `now` and synthetic messages.

use std::time::SystemTime;

use crossterm::event::{KeyCode, KeyModifiers};

use super::model::{Effect, Model, Msg};

/// Advance the model by one message.
///
/// Returns the new model and an optional effect for the shell to execute.
#[must_use]
pub fn update(mut model: Model, msg: Msg, now: SystemTime) -> (Model, Option<Effect>) {
    match msg {
        Msg::Key(key) => handle_key(model, key, now),
        Msg::Tick => {
            // Skip the periodic fetch if one is already in flight, so a slow or
            // hung master cannot pile up overlapping requests. A manual `r`
            // still forces a refresh.
            if model.loading {
                (model, None)
            } else {
                model.loading = true;
                (model, Some(Effect::FetchAgents))
            }
        }
        Msg::AgentsLoaded(Ok(agents)) => {
            model.agents = agents;
            model.loading = false;
            model.last_error = None;
            model.last_fetch = Some(now);
            if model.agents.is_empty() {
                model.selected = 0;
            } else {
                model.selected = model.selected.min(model.agents.len() - 1);
            }
            (model, None)
        }
        Msg::AgentsLoaded(Err(e)) => {
            model.loading = false;
            model.last_error = Some(e.to_string());
            (model, None)
        }
    }
}

fn handle_key(
    mut model: Model,
    key: crossterm::event::KeyEvent,
    now: SystemTime,
) -> (Model, Option<Effect>) {
    use KeyCode::{Char, Down, Up};

    if matches!(key.code, Char('q'))
        || matches!(key.code, Char('c') if key.modifiers.contains(KeyModifiers::CONTROL))
    {
        return (model, Some(Effect::Quit));
    }

    if matches!(key.code, Char('r')) {
        model.loading = true;
        return (model, Some(Effect::FetchAgents));
    }

    let agent_count = model.agents.len();
    if matches!(key.code, Down) && agent_count > 0 {
        model.selected = (model.selected + 1).min(agent_count - 1);
    }
    if matches!(key.code, Up) && model.selected > 0 {
        model.selected -= 1;
    }

    let _ = now; // now is not used in key handling but kept for signature consistency
    (model, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{Agent, ApiError};
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
    use std::time::{Duration, UNIX_EPOCH};

    fn now() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(100)
    }

    fn key(code: KeyCode) -> Msg {
        Msg::Key(KeyEvent {
            code,
            modifiers: KeyModifiers::empty(),
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        })
    }

    fn agent(hostname: &str) -> Agent {
        Agent {
            hostname: hostname.to_string(),
            name: hostname.to_string(),
            last_seen: UNIX_EPOCH,
            tainted: false,
        }
    }

    #[test]
    fn q_key_emits_quit_effect() {
        let model = Model::new();
        let (_, effect) = update(model, key(KeyCode::Char('q')), now());
        assert_eq!(effect, Some(Effect::Quit));
    }

    #[test]
    fn r_key_emits_fetch_agents_effect() {
        let model = Model::new();
        let (m, effect) = update(model, key(KeyCode::Char('r')), now());
        assert_eq!(effect, Some(Effect::FetchAgents));
        assert!(m.loading);
    }

    #[test]
    fn tick_emits_fetch_agents_effect() {
        let model = Model::new();
        let (m, effect) = update(model, Msg::Tick, now());
        assert_eq!(effect, Some(Effect::FetchAgents));
        assert!(m.loading);
    }

    #[test]
    fn tick_while_loading_does_not_refetch() {
        let mut model = Model::new();
        model.loading = true;
        let (m, effect) = update(model, Msg::Tick, now());
        assert_eq!(effect, None);
        assert!(m.loading);
    }

    #[test]
    fn agents_loaded_ok_updates_agents_and_clears_error() {
        let mut model = Model::new();
        model.last_error = Some("old error".to_string());
        let (m, effect) = update(model, Msg::AgentsLoaded(Ok(vec![agent("web-01")])), now());
        assert_eq!(effect, None);
        assert_eq!(m.agents.len(), 1);
        assert_eq!(m.agents[0].hostname, "web-01");
        assert!(m.last_error.is_none());
        assert!(!m.loading);
        assert!(m.last_fetch.is_some());
    }

    #[test]
    fn agents_loaded_err_sets_last_error() {
        let model = Model::new();
        let (m, effect) = update(model, Msg::AgentsLoaded(Err(ApiError::NotFound)), now());
        assert_eq!(effect, None);
        assert!(m.last_error.is_some());
        assert!(!m.loading);
    }

    #[test]
    fn down_key_moves_selection() {
        let mut model = Model::new();
        model.agents = vec![agent("a"), agent("b"), agent("c")];
        model.selected = 0;
        let (m, _) = update(model, key(KeyCode::Down), now());
        assert_eq!(m.selected, 1);
    }

    #[test]
    fn down_key_clamps_at_last() {
        let mut model = Model::new();
        model.agents = vec![agent("a"), agent("b")];
        model.selected = 1;
        let (m, _) = update(model, key(KeyCode::Down), now());
        assert_eq!(m.selected, 1);
    }

    #[test]
    fn up_key_moves_selection() {
        let mut model = Model::new();
        model.agents = vec![agent("a"), agent("b")];
        model.selected = 1;
        let (m, _) = update(model, key(KeyCode::Up), now());
        assert_eq!(m.selected, 0);
    }

    #[test]
    fn up_key_clamps_at_zero() {
        let mut model = Model::new();
        model.agents = vec![agent("a")];
        model.selected = 0;
        let (m, _) = update(model, key(KeyCode::Up), now());
        assert_eq!(m.selected, 0);
    }

    #[test]
    fn agents_loaded_ok_clamps_selection() {
        let mut model = Model::new();
        model.agents = vec![agent("a"), agent("b"), agent("c")];
        model.selected = 2;
        let (m, _) = update(model, Msg::AgentsLoaded(Ok(vec![agent("x")])), now());
        assert_eq!(m.selected, 0);
    }
}
