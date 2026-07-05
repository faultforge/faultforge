//! Interactive TUI shell: terminal setup/teardown and the async event loop.
//!
//! This is the imperative shell — all I/O lives here. Decision logic is in
//! `update`; rendering is in `view`.

pub mod model;
pub mod update;
pub mod view;

use std::io;
use std::panic;
use std::time::{Duration, SystemTime};

use crossterm::{
    event::{self, Event, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use tokio::{sync::mpsc, time::interval};

use crate::api::Client;
use model::{Effect, Model, Msg};

/// Polling interval between automatic agent refreshes.
const POLL_INTERVAL: Duration = Duration::from_secs(3);

/// Spawn a thread that reads crossterm events and sends key presses into `tx`.
///
/// The returned handle is detached on drop; the thread exits on its own when
/// the channel closes (the event loop dropped the receiver).
fn spawn_key_reader(tx: mpsc::UnboundedSender<Msg>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        loop {
            match event::read() {
                Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                    if tx.send(Msg::Key(key)).is_err() {
                        // Receiver dropped — main loop exited; stop the thread.
                        break;
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    })
}

/// Launch the interactive TUI.
///
/// Sets up the terminal, runs the event loop, and restores the terminal on exit.
///
/// # Errors
///
/// Returns an error if the terminal cannot be set up or if rendering fails.
pub async fn run(client: Client) -> anyhow::Result<()> {
    let mut terminal = setup_terminal()?;

    // Install a panic hook so that the terminal is always restored even if a
    // thread panics mid-render.
    let original_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stderr(), LeaveAlternateScreen);
        original_hook(info);
    }));

    let result = event_loop(&mut terminal, client).await;

    restore_terminal(&mut terminal)?;
    result
}

/// Set up raw mode and alternate screen, return a ratatui terminal.
fn setup_terminal() -> anyhow::Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(io::stdout());
    Ok(Terminal::new(backend)?)
}

/// Restore the terminal to its normal state.
fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> anyhow::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

/// The main event loop: render → await event → update → handle effects.
async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    client: Client,
) -> anyhow::Result<()> {
    let mut model = Model::new();
    let mut ticker = interval(POLL_INTERVAL);
    // Drain the first immediate tick so we fire an initial fetch without waiting 3s.
    ticker.tick().await;

    // Single channel for all messages: key events from the reader thread, and
    // fetch completions from spawned async tasks.
    let (tx, mut rx) = mpsc::unbounded_channel::<Msg>();

    let _key_reader = spawn_key_reader(tx.clone());

    // Fetch once up front so the UI isn't empty before the first poll tick.
    spawn_fetch(client.clone(), tx.clone());
    model.loading = true;

    loop {
        let now = SystemTime::now();

        terminal.draw(|frame| view::view(&model, now, frame))?;

        let msg = tokio::select! {
            Some(msg) = rx.recv() => msg,
            _ = ticker.tick() => Msg::Tick,
        };

        let (new_model, effect) = update::update(model, msg, now);
        model = new_model;

        match effect {
            Some(Effect::Quit) => break,
            Some(Effect::FetchAgents) => {
                spawn_fetch(client.clone(), tx.clone());
            }
            None => {}
        }
    }

    Ok(())
}

/// Spawn a background task to fetch agents and send the result into `tx`.
fn spawn_fetch(client: Client, tx: mpsc::UnboundedSender<Msg>) {
    tokio::spawn(async move {
        let result = client.list_agents().await;
        let _ = tx.send(Msg::AgentsLoaded(result));
    });
}
