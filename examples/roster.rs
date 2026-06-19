//! Non-interactive smoke test for the vendored data layer.
//!
//! Mirrors the discovery half of `claudectl_tui::app::App::refresh` and prints
//! the live Claude Code session roster. This proves the Phase 0 fork runs
//! against real `~/.claude` data without needing an interactive TTY (which the
//! full TUI binary requires). Run: `cargo run --example roster`.

use claudectl_core::{discovery, monitor, process};

fn main() {
    let mut sessions = discovery::scan_sessions();
    println!(
        "scanned {} — found {} session file(s)",
        discovery::projects_dir().display(),
        sessions.len()
    );

    // Same sequence App::refresh runs: ps enrichment (drops dead PIDs) →
    // JSONL path resolution → subagent scan → incremental token/status parse.
    process::fetch_and_enrich(&mut sessions);
    for s in &mut sessions {
        if s.jsonl_path.is_none() {
            discovery::resolve_jsonl_paths(std::slice::from_mut(s));
        }
    }
    discovery::scan_subagents(&mut sessions);
    for s in &mut sessions {
        monitor::update_tokens(s); // parses JSONL deltas + calls infer_status
    }

    if sessions.is_empty() {
        println!("(no live claude sessions — start one and re-run)");
        return;
    }

    println!(
        "\n{:<26}  {:<9}  {:>7}  {:>8}  {:>5}",
        "PROJECT / AGENT", "STATUS", "PID", "COST", "CTX%"
    );
    println!("{}", "-".repeat(64));
    for s in &sessions {
        println!(
            "{:<26}  {:<9}  {:>7}  {:>8}  {:>4.0}%",
            truncate(s.display_name(), 26),
            s.status, // SessionStatus: Display
            s.pid,
            s.format_cost(),
            s.context_percent(),
        );
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n - 1).collect::<String>())
    }
}
