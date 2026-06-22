//! Pure TUI view: build all four layout regions from the model.
//!
//! No I/O — `view` returns a closure that ratatui can render.

use std::time::SystemTime;

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};

use super::model::{Model, Screen};
use crate::output::relative_age;

/// Render the full four-region layout into `frame`.
///
/// `now` is passed in from the shell so all relative-age strings are deterministic.
pub fn view(model: &Model, now: SystemTime, frame: &mut Frame<'_>) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // top health strip
            Constraint::Min(0),    // main area
            Constraint::Length(1), // bottom shortcut bar
        ])
        .split(area);

    render_top_strip(model, now, frame, chunks[0]);
    render_main(model, now, frame, chunks[1]);
    render_bottom_bar(model, frame, chunks[2]);
}

fn render_top_strip(model: &Model, now: SystemTime, frame: &mut Frame<'_>, area: Rect) {
    let online = model.agents.len();

    // An error is the most important state to surface, so it wins over the
    // "Refreshing…" hint and is coloured red. Before the first successful fetch
    // we show "Connecting…" rather than a misleading "0 agents".
    let (summary, summary_style) = if let Some(ref err) = model.last_error {
        (format!("Error: {err}"), Style::default().fg(Color::Red))
    } else if model.last_fetch.is_none() {
        (
            "Connecting to master…".to_string(),
            Style::default().fg(Color::Yellow),
        )
    } else if model.loading {
        ("Refreshing…".to_string(), Style::default())
    } else {
        (format!("{online} agent(s) registered"), Style::default())
    };

    let last = model
        .last_fetch
        .map(|t| format!("  [last refresh: {}]", relative_age(now, t)))
        .unwrap_or_default();

    let line = Line::from(vec![Span::styled(summary, summary_style), Span::raw(last)]);
    let para =
        Paragraph::new(line).block(Block::default().borders(Borders::ALL).title("FaultForge"));
    frame.render_widget(para, area);
}

fn render_main(model: &Model, now: SystemTime, frame: &mut Frame<'_>, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(16), // left menu
            Constraint::Min(0),     // right detail pane
        ])
        .split(area);

    render_left_menu(model, frame, chunks[0]);
    render_right_pane(model, now, frame, chunks[1]);
}

fn render_left_menu(model: &Model, frame: &mut Frame<'_>, area: Rect) {
    let items: Vec<ListItem<'_>> = vec![ListItem::new(screen_label(&Screen::Agents))];

    let selected_idx = match model.screen {
        Screen::Agents => 0,
    };

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Menu"))
        .highlight_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");

    let mut state = ListState::default();
    state.select(Some(selected_idx));
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_right_pane(model: &Model, now: SystemTime, frame: &mut Frame<'_>, area: Rect) {
    match model.screen {
        Screen::Agents => render_agents_pane(model, now, frame, area),
    }
}

fn render_agents_pane(model: &Model, now: SystemTime, frame: &mut Frame<'_>, area: Rect) {
    let items: Vec<ListItem<'_>> = model
        .agents
        .iter()
        .map(|a| {
            let age = relative_age(now, a.last_seen);
            let dot = online_dot(now, a.last_seen);
            let line = Line::from(vec![
                Span::styled(dot, Style::default().fg(status_color(now, a.last_seen))),
                Span::raw(format!(" {:<20} {}", a.name, age)),
            ]);
            ListItem::new(line)
        })
        .collect();

    let title = format!("Agents ({})", model.agents.len());
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("  ");

    let mut state = ListState::default();
    if !model.agents.is_empty() {
        state.select(Some(model.selected));
    }
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_bottom_bar(model: &Model, frame: &mut Frame<'_>, area: Rect) {
    let shortcuts = shortcuts_for_screen(&model.screen);
    let para = Paragraph::new(shortcuts).style(Style::default().fg(Color::DarkGray));
    frame.render_widget(para, area);
}

fn screen_label(screen: &Screen) -> &'static str {
    match screen {
        Screen::Agents => "Agents",
    }
}

fn shortcuts_for_screen(screen: &Screen) -> &'static str {
    match screen {
        Screen::Agents => "q quit  r refresh  ↑↓ select",
    }
}

/// A visual status dot for an agent.
fn online_dot(now: SystemTime, last_seen: SystemTime) -> &'static str {
    match now.duration_since(last_seen) {
        Ok(d) if d.as_secs() <= 30 => "●",
        _ => "○",
    }
}

/// Colour for the status dot.
fn status_color(now: SystemTime, last_seen: SystemTime) -> Color {
    match now.duration_since(last_seen) {
        Ok(d) if d.as_secs() <= 30 => Color::Green,
        _ => Color::Red,
    }
}
