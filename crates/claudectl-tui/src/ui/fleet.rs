//! Phase 5 fleet trend strip (CLAUDE.md §5/§7) — a one-line, whole-fleet glance
//! beneath the roster. The roster already shows each agent's *current* state and
//! a per-row activity sparkline; this strip adds the two things a row can't: the
//! **time axis** (a sparkline of total fleet burn over recent ticks) and
//! **cross-session history** (today/week cost from `history::weekly_summary`).
//! Toggle with `G`. Data is already accounted for upstream — no new sources.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::app::App;

/// Render the fleet strip into a single-row `area`. Assumes the caller only draws
/// it when `app.show_fleet` and there are agents to summarize.
pub fn render_fleet_strip(frame: &mut Frame, area: Rect, app: &App) {
    let t = &app.theme;
    let c = app.fleet_counts();
    let dim = Style::default().fg(t.text_muted);

    let count = |label: &'static str, n: usize, color| {
        // Dim a zero so the eye lands on the statuses that actually have agents.
        let style = if n == 0 {
            dim.add_modifier(Modifier::DIM)
        } else {
            Style::default().fg(color)
        };
        Span::styled(format!("{label} {n}"), style)
    };

    let mut spans = vec![
        Span::styled(" Fleet  ", Style::default().fg(t.header).add_modifier(Modifier::BOLD)),
        count("needs", c.needs_input, t.status_needs_input),
        Span::styled("  ", dim),
        count("proc", c.processing, t.status_processing),
        Span::styled("  ", dim),
        count("wait", c.waiting, t.status_waiting),
        Span::styled("  ", dim),
        count("idle", c.idle, t.status_idle),
        Span::styled("   \u{2502}  ", dim), // │ separator
        Span::styled(
            format!("burn ${:.2}/hr ", c.burn_per_hr),
            Style::default().fg(t.text_primary),
        ),
        Span::styled(spark(&app.fleet_burn_history), Style::default().fg(t.sparkline)),
    ];

    // Cross-session history (not derivable from the live roster at all).
    // today/wk cost + wk tokens (BACKLOG "wk token tracker"), then the all-time
    // collection totals (BACKLOG "Persistent cost").
    let w = &app.weekly_summary;
    let a = &app.all_time_summary;
    spans.push(Span::styled(
        format!(
            "   \u{2502}  today ${:.2} \u{00b7} wk ${:.2} ({})",
            w.today_cost_usd,
            w.cost_usd,
            fmt_tokens(w.total_tokens),
        ),
        dim,
    ));
    spans.push(Span::styled(
        format!(
            "   \u{2502}  all ${:.2} ({})",
            a.cost_usd,
            fmt_tokens(a.total_tokens),
        ),
        dim,
    ));

    // Daily activity heatmap (last ACTIVITY_DAYS days): one cell per day, shaded by
    // that day's spend relative to the busiest day — the "days" time axis the live
    // burn sparkline (last ~30s) can't show.
    if !app.daily_activity.is_empty() {
        spans.push(Span::styled(
            format!("   \u{2502}  {}d ", app.daily_activity.len()),
            dim,
        ));
        spans.push(Span::styled(
            heatmap(&app.daily_activity),
            Style::default().fg(t.sparkline),
        ));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Map a per-day intensity series (0..=4) to a heatmap row. Level 0 reads as an empty
/// day; higher levels shade progressively darker (· ░ ▒ ▓ █).
fn heatmap(levels: &[u8]) -> String {
    const CELLS: [char; 5] = ['\u{00b7}', '\u{2591}', '\u{2592}', '\u{2593}', '\u{2588}'];
    levels
        .iter()
        .map(|&l| CELLS[(l as usize).min(CELLS.len() - 1)])
        .collect()
}

/// Compact token count for the strip: "1.2M tok" / "450k tok" / "37 tok".
fn fmt_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M tok", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.0}k tok", n as f64 / 1_000.0)
    } else {
        format!("{n} tok")
    }
}

/// Render a slice of values as a unicode block sparkline, scaled to the slice's
/// own max. Empty → a single dash; an all-zero history → flat low bars (idle
/// fleet reads as quiet, not blank).
fn spark(history: &[f64]) -> String {
    const BLOCKS: &[char] = &[
        '\u{2581}', '\u{2582}', '\u{2583}', '\u{2584}', '\u{2585}', '\u{2586}', '\u{2587}',
        '\u{2588}',
    ];
    if history.is_empty() {
        return "-".to_string();
    }
    let max = history.iter().cloned().fold(0.0_f64, f64::max);
    if max <= 0.0 {
        return BLOCKS[0].to_string().repeat(history.len());
    }
    history
        .iter()
        .map(|&v| {
            let idx = ((v / max) * (BLOCKS.len() - 1) as f64).round() as usize;
            BLOCKS[idx.min(BLOCKS.len() - 1)]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{fmt_tokens, heatmap, spark};

    #[test]
    fn heatmap_maps_levels_to_shades() {
        assert_eq!(heatmap(&[0, 1, 2, 3, 4]), "\u{00b7}\u{2591}\u{2592}\u{2593}\u{2588}");
        assert_eq!(heatmap(&[]), "");
        // Out-of-range levels clamp to the darkest cell rather than panicking.
        assert_eq!(heatmap(&[9]), "\u{2588}");
    }

    #[test]
    fn fmt_tokens_scales_units() {
        assert_eq!(fmt_tokens(37), "37 tok");
        assert_eq!(fmt_tokens(45_000), "45k tok");
        assert_eq!(fmt_tokens(1_500_000), "1.5M tok");
    }

    #[test]
    fn empty_history_is_a_dash() {
        assert_eq!(spark(&[]), "-");
    }

    #[test]
    fn all_zero_history_is_flat_low_bars() {
        assert_eq!(spark(&[0.0, 0.0, 0.0]), "\u{2581}\u{2581}\u{2581}");
    }

    #[test]
    fn scales_to_own_max() {
        // Max maps to the tallest block, zero to the shortest, midpoint between.
        let s = spark(&[0.0, 5.0, 10.0]);
        let chars: Vec<char> = s.chars().collect();
        assert_eq!(chars.len(), 3);
        assert_eq!(chars[0], '\u{2581}');
        assert_eq!(chars[2], '\u{2588}');
        assert!(chars[1] > '\u{2581}' && chars[1] < '\u{2588}');
    }
}
