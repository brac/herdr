//! Phase 4c approval inspector (PHASE4_PLAN.md Part B): a centered modal that
//! shows the *actual* permission dialog scraped from the agent's tmux pane
//! (`capture-pane` — the prompt never reaches the JSONL) above one-key
//! approve/deny/interrupt actions, so you can act without switching panes.
//! Read-only scrape, not a terminal emulator (CLAUDE.md §0.1, §8).

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

use super::help::centered_rect;
use crate::app::App;
use claudectl_core::session::SessionStatus;

/// Render the approval inspector over the roster. Assumes `app.approval_pid` is
/// `Some`; the caller gates on that.
pub fn render_approval_modal(frame: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let t = &app.theme;
    let popup = centered_rect(70, 70, area);
    frame.render_widget(Clear, popup);

    let session = app
        .approval_pid
        .and_then(|pid| app.sessions.iter().find(|s| s.pid == pid));

    let title = match session {
        Some(s) => format!(" approve? \u{2014} {} (PID {}) ", s.display_name(), s.pid),
        None => " approve? ".to_string(),
    };
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(t.header));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let Some(session) = session else {
        frame.render_widget(
            Paragraph::new("Agent is no longer running. Press Esc.")
                .style(Style::default().fg(t.text_muted)),
            inner,
        );
        return;
    };

    // Layout: a one-line status hint, the captured dialog (fills the middle), and
    // a one-line action legend pinned to the bottom.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);

    // Status hint: most useful when the agent is actually blocked; otherwise note
    // that approve is a no-op but deny/interrupt still reach the pane.
    let hint = if session.status == SessionStatus::NeedsInput {
        Span::styled(
            "  Blocked on a permission prompt \u{2014} pick an action below.",
            Style::default().fg(t.status_needs_input),
        )
    } else {
        Span::styled(
            "  Not currently blocked \u{2014} deny/interrupt still send Esc to the pane.",
            Style::default().fg(t.text_muted),
        )
    };
    frame.render_widget(Paragraph::new(Line::from(hint)), chunks[0]);

    // Captured dialog. The prompt sits at the bottom of the pane, so show the tail
    // that fits (after trimming trailing blank lines from the capture).
    let body_h = chunks[1].height as usize;
    let preview: Vec<Line> = if app.approval_preview.trim().is_empty() {
        vec![Line::from(Span::styled(
            "  (no pane capture \u{2014} press r to retry, or act blind)",
            Style::default().fg(t.text_muted),
        ))]
    } else {
        let lines: Vec<&str> = app
            .approval_preview
            .lines()
            .rev()
            .skip_while(|l| l.trim().is_empty())
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let start = lines.len().saturating_sub(body_h);
        lines[start..]
            .iter()
            .map(|l| {
                Line::from(Span::styled(
                    (*l).to_string(),
                    Style::default().fg(t.text_primary),
                ))
            })
            .collect()
    };
    frame.render_widget(Paragraph::new(preview), chunks[1]);

    // Action legend.
    let key = |k: &'static str| Span::styled(k, Style::default().fg(t.highlight_key));
    let legend = Line::from(vec![
        Span::raw("  "),
        key("y"),
        Span::raw(" approve   "),
        key("n"),
        Span::raw(" deny   "),
        key("i"),
        Span::raw(" interrupt   "),
        key("r"),
        Span::raw(" re-capture   "),
        key("Esc"),
        Span::raw(" cancel"),
    ]);
    frame.render_widget(
        Paragraph::new(legend).style(Style::default().add_modifier(Modifier::BOLD)),
        chunks[2],
    );
}
