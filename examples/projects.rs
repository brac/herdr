//! Non-interactive proof of the Phase 1 project-first roster.
//!
//! Builds the App against a parent dir (default current dir, or argv[1]),
//! and prints the project roster: every project appears whether or not it
//! hosts agents, with live agents nested under their project. TTY-free, so it
//! validates the §2 inversion without driving the full TUI.
//! Run: `cargo run --example projects -- ~/Work`

use std::path::PathBuf;

use claudectl_tui::app::App;

fn main() {
    let parent = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().expect("cwd"));

    // with_parent() scans the parent for projects and discovers live sessions.
    let app = App::with_parent(parent.clone());
    let groups = app.project_groups();
    let sessions = app.visible_sessions();

    let active = groups.iter().filter(|g| g.session_count > 0).count();
    println!(
        "{} — {} projects, {} with live agents\n",
        parent.display(),
        groups.len(),
        active
    );

    let mut idle: Vec<&str> = Vec::new();
    for g in &groups {
        if g.session_count == 0 {
            idle.push(&g.name);
            continue;
        }
        println!(
            "▸ {}  —  {} agent(s), {} active, ${:.2}, ctx {:.0}%",
            g.name, g.session_count, g.active_count, g.total_cost, g.avg_context_pct
        );
        for pid in &g.pids {
            if let Some(s) = sessions.iter().find(|s| s.pid == *pid) {
                println!("    └ {:<7} {:<11} {}", s.pid, s.status, s.format_cost());
            }
        }
    }

    if !idle.is_empty() {
        println!("\nidle projects ({}): {}", idle.len(), idle.join(", "));
    }
}
