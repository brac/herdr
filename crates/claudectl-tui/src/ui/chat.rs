//! Phase 4 conversation view (CLAUDE.md §5) — oatmeal-style chat bubbles for the
//! selected agent: assistant text on the **left**, your messages on the **right**,
//! tool calls as dim left-aligned lines. Read-only in 4a; input + approve/deny
//! control come in 4b/4c. Content is sourced from `ClaudeSession.conversation`,
//! which `monitor::update_tokens` fills incrementally from the JSONL transcript.

use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use crate::app::App;
use claudectl_core::session::{ChatKind, ChatRole};

pub fn render_chat(frame: &mut Frame, area: Rect, app: &App) {
    let t = &app.theme;
    let session = app
        .chat_pid
        .and_then(|pid| app.sessions.iter().find(|s| s.pid == pid));

    let title = match session {
        Some(s) => format!(
            " chat · {} (PID {})  —  type · Enter send · ↑↓ scroll · Esc close ",
            s.display_name(),
            s.pid
        ),
        None => " chat ".to_string(),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(t.border));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(session) = session else {
        frame.render_widget(
            Paragraph::new("Agent is no longer running.").style(Style::default().fg(t.text_muted)),
            inner,
        );
        return;
    };

    // Reserve the bottom two rows for a status line and the reply box; the
    // conversation fills the rest. The status line surfaces send results/errors
    // (otherwise invisible, since the chat overlay hides the roster status bar).
    let footer_h = 2u16.min(inner.height);
    let convo = Rect {
        height: inner.height.saturating_sub(footer_h),
        ..inner
    };
    let status_area = Rect {
        y: inner.y + convo.height,
        height: footer_h.saturating_sub(1),
        ..inner
    };
    let input_area = Rect {
        y: inner.y + convo.height + status_area.height,
        height: footer_h.min(1),
        ..inner
    };

    // Bubbles take ~70% of the width so left/right alignment reads as a chat.
    let width = convo.width.max(10) as usize;
    let bubble_w = (width * 7 / 10).max(8);

    let mut lines: Vec<Line> = Vec::new();
    for msg in &session.conversation {
        let (style, align, prefix) = match (msg.role, msg.kind) {
            (ChatRole::Assistant, ChatKind::Tool) => (
                Style::default().fg(t.text_muted).add_modifier(Modifier::DIM),
                Alignment::Left,
                "⚙ ",
            ),
            (ChatRole::Assistant, _) => {
                (Style::default().fg(t.text_primary), Alignment::Left, "")
            }
            (ChatRole::User, _) => (Style::default().fg(t.input_accent), Alignment::Right, ""),
        };
        for (i, wrapped) in wrap(&msg.text, bubble_w).into_iter().enumerate() {
            let content = if i == 0 && !prefix.is_empty() {
                format!("{prefix}{wrapped}")
            } else {
                wrapped
            };
            lines.push(Line::from(Span::styled(content, style)).alignment(align));
        }
        lines.push(Line::from("")); // blank line between messages
    }

    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "No conversation captured yet — waiting for transcript activity.",
            Style::default().fg(t.text_muted),
        )));
    }

    // Scroll: `chat_scroll` counts lines back from the newest. 0 pins the view to
    // the bottom (newest), so a live agent's messages appear as they arrive.
    let total = lines.len() as u16;
    let max_off = total.saturating_sub(convo.height);
    let scroll = app.chat_scroll.min(max_off);
    let offset = max_off.saturating_sub(scroll);

    frame.render_widget(Paragraph::new(lines).scroll((offset, 0)), convo);

    // Status line: surface the latest send result/error so failures aren't
    // silent. Tint red when it reads like a failure.
    if status_area.height > 0 && !app.status_msg.is_empty() {
        let s = &app.status_msg;
        let color = if s.contains("fail") || s.contains("Failed") || s.contains("not ") {
            t.error
        } else {
            t.text_muted
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(format!("  {s}"), Style::default().fg(color)))),
            status_area,
        );
    }

    // Reply box (Phase 4b): always focused — type a prompt, Enter sends.
    let input_line = Line::from(vec![
        Span::styled("> ", Style::default().fg(t.input_accent)),
        Span::raw(app.chat_input.as_str()),
        Span::styled("▏", Style::default().fg(t.input_accent)),
    ]);
    frame.render_widget(Paragraph::new(input_line), input_area);
}

/// Simple greedy word-wrap to `width` columns (no dependency). Preserves
/// explicit newlines; very long words overflow rather than being split.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    for raw in text.split('\n') {
        if raw.trim().is_empty() {
            out.push(String::new());
            continue;
        }
        let mut cur = String::new();
        for word in raw.split_whitespace() {
            if cur.is_empty() {
                cur.push_str(word);
            } else if cur.chars().count() + 1 + word.chars().count() <= width {
                cur.push(' ');
                cur.push_str(word);
            } else {
                out.push(std::mem::take(&mut cur));
                cur.push_str(word);
            }
        }
        if !cur.is_empty() {
            out.push(cur);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::wrap;

    #[test]
    fn wraps_to_width_and_keeps_newlines() {
        let w = wrap("the quick brown fox\njumps", 9);
        assert_eq!(w, vec!["the quick", "brown fox", "jumps"]);
    }

    #[test]
    fn empty_text_yields_one_blank_line() {
        assert_eq!(wrap("", 10), vec![String::new()]);
    }
}
