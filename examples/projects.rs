//! Non-interactive proof of the project-first roster (Phases 1 and 2).
//!
//! Builds the App against a parent dir (default current dir, or argv[1]), and
//! prints the project roster: every project appears whether or not it hosts
//! agents, with live agents nested under their project (Phase 1). It then prints
//! the *navigable* roster — the rows the cursor can land on — to show that
//! project headers, including idle zero-agent ones, are selectable so `n` can
//! launch an agent into any of them (Phase 2). TTY-free.
//! Run: `cargo run --example projects -- ~/Work`

use std::path::PathBuf;

use claudectl_tui::app::{App, RosterRow};

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

    // Phase 2 (CLAUDE.md §2): the navigable roster — every row the cursor can
    // land on. Headers (even idle, zero-agent projects) are selectable rows, so
    // `n` launches an agent into wherever the cursor sits.
    let (groups, rows) = app.roster_layout();
    println!("\nnavigable roster — {} selectable rows:", rows.len());
    for (i, row) in rows.iter().enumerate() {
        match row {
            RosterRow::Header(gi) => {
                let g = &groups[*gi];
                let kind = if g.session_count == 0 {
                    "project (idle)"
                } else {
                    "project"
                };
                // Phase 3 light path: show the git glance per project header.
                let git = match &g.git {
                    None => String::new(),
                    Some(s) if s.bare => "  (bare)".to_string(),
                    Some(s) => {
                        let dirty = if s.dirty { " ●" } else { "" };
                        let ahead = if s.upstream && s.ahead > 0 {
                            format!(" ↑{}", s.ahead)
                        } else {
                            String::new()
                        };
                        let behind = if s.upstream && s.behind > 0 {
                            format!(" ↓{}", s.behind)
                        } else {
                            String::new()
                        };
                        format!("  [{}{dirty}{ahead}{behind}]", s.branch)
                    }
                };
                println!("  [{i:>2}] {kind:<14} {}{git}", g.name);
            }
            RosterRow::Agent(si) => {
                if let Some(s) = app.sessions.get(*si) {
                    println!("  [{i:>2}] {:<14} └ {} {}", "agent", s.pid, s.status);
                }
            }
        }
    }
}
