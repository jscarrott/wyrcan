//! Colours and glyphs. One accent, dim secondary text.
use crate::model::AgentState;
use ratatui::style::{Color, Modifier, Style};

/// Todoist red: the selection, and the header's blocked count.
pub const ACCENT: Color = Color::Rgb(220, 76, 62);
pub const DIM: Color = Color::DarkGray;

pub fn dim() -> Style {
    Style::new().fg(DIM)
}

pub fn bold() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

pub fn chip() -> Style {
    Style::new().bg(DIM).fg(Color::White)
}

/// Status glyph, name and colour of an agent state.
pub fn agent(s: AgentState) -> (&'static str, &'static str, Color) {
    match s {
        AgentState::Working => ("●", "working", Color::Green),
        AgentState::Waiting => ("◆", "waiting", Color::Yellow),
        AgentState::Blocked => ("■", "blocked", Color::Red),
        AgentState::Ended => ("○", "ended", DIM),
    }
}

/// `◉` for a live session, `○` for none, coloured by state.
pub fn circle(s: Option<AgentState>) -> (&'static str, Color) {
    match s {
        Some(st) if st != AgentState::Ended => ("◉", agent(st).2),
        _ => ("○", DIM),
    }
}
