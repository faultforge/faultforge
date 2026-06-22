//! Pure TUI model: all state, messages, and effects.

use std::time::SystemTime;

use crate::api::{Agent, ApiError};

/// The full UI state.
#[derive(Debug, Clone)]
pub struct Model {
    /// Agents last loaded from the master.
    pub agents: Vec<Agent>,
    /// Currently active section.
    pub screen: Screen,
    /// Index of the selected row in the right pane (0-based, clamped to agent count).
    pub selected: usize,
    /// Whether a fetch is currently in flight.
    pub loading: bool,
    /// Last fetch error, shown in the UI without crashing.
    pub last_error: Option<String>,
    /// When the agents list was last successfully fetched.
    pub last_fetch: Option<SystemTime>,
}

impl Model {
    /// Construct an initial empty model.
    #[must_use]
    pub fn new() -> Self {
        Self {
            agents: Vec::new(),
            screen: Screen::Agents,
            selected: 0,
            loading: false,
            last_error: None,
            last_fetch: None,
        }
    }
}

impl Default for Model {
    fn default() -> Self {
        Self::new()
    }
}

/// Available TUI sections.
///
/// Only `Agents` exists in this slice; new sections become new variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Screen {
    Agents,
}

/// Messages that drive the update loop (inputs and async completions).
#[derive(Debug)]
pub enum Msg {
    /// A keyboard event from crossterm.
    Key(crossterm::event::KeyEvent),
    /// A polling-interval tick — triggers a background fetch.
    Tick,
    /// The result of a `FetchAgents` effect completing.
    AgentsLoaded(Result<Vec<Agent>, ApiError>),
}

/// Side-effects requested by `update`; executed by the shell.
#[derive(Debug, PartialEq, Eq)]
pub enum Effect {
    /// Fetch agents from the master and feed the result back as `AgentsLoaded`.
    FetchAgents,
    /// Exit the TUI loop.
    Quit,
}
