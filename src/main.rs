//! herdr — a project-first terminal cockpit for Claude Code agents.
//!
//! Phase 0 fork (CLAUDE.md §7). This binary drives claudectl's vendored data
//! layer (`claudectl-core`) and roster TUI (`claudectl-tui`) with a minimal
//! synchronous poll loop. Upstream's brain / bus / coord orchestrator is
//! intentionally absent: `App::new()` constructs an in-memory `MockRuntime`,
//! so every piece of live state comes from direct `~/.claude` session
//! discovery — no async runtime, no SQLite, no MCP server.
//!
//! Phase 1 (CLAUDE.md §2): the roster is project-first. herdr scans the parent
//! directory it is launched from for project repos and nests each project's
//! live agents beneath it. Usage: `herdr [--all] [PARENT_DIR]`.
#![allow(clippy::collapsible_if)] // keep the nested poll/read/handle_key block flat, as upstream does

use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::{
    event::{self, Event},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};

use claudectl_tui::{app::App, ui};

/// Poll cadence. Local filesystem reads are <1ms, so a 2s tick keeps the
/// roster fresh without busy-spinning (CLAUDE.md §3; matches upstream).
const TICK_RATE: Duration = Duration::from_secs(2);

fn main() -> io::Result<()> {
    let opts = Options::from_args();
    install_panic_restore();
    let mut terminal = enter_tui()?;
    let result = run(&mut terminal, opts);
    leave_tui(&mut terminal)?;
    result
}

/// Minimal CLI: `herdr [--all] [PARENT_DIR]`. No clap — a single optional path
/// and one flag don't justify the dependency (CLAUDE.md §3).
struct Options {
    /// Parent directory to scan for projects (default: current dir).
    parent: PathBuf,
    /// Include non-git subdirectories in the project roster (default: .git only).
    include_non_git: bool,
}

impl Options {
    fn from_args() -> Self {
        let mut parent: Option<PathBuf> = None;
        let mut include_non_git = false;
        for arg in std::env::args().skip(1) {
            match arg.as_str() {
                "--all" | "-a" => include_non_git = true,
                _ if parent.is_none() => parent = Some(PathBuf::from(arg)),
                _ => {}
            }
        }
        let parent = parent
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        Self {
            parent,
            include_non_git,
        }
    }
}

/// Install a panic hook that restores the terminal before the default handler
/// prints. A crashed TUI must never leave the terminal in raw/alt-screen mode
/// (CLAUDE.md §3). `panic = "abort"` means this hook is our only restore shot.
fn install_panic_restore() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        default_hook(info);
    }));
}

fn enter_tui() -> io::Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    Terminal::new(CrosstermBackend::new(stdout))
}

fn leave_tui<W: io::Write>(terminal: &mut Terminal<CrosstermBackend<W>>) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()
}

/// The synchronous poll loop: draw → wait for input up to the next tick →
/// refresh on tick. Returns when the user quits (`handle_key` returns false).
fn run<W: io::Write>(
    terminal: &mut Terminal<CrosstermBackend<W>>,
    opts: Options,
) -> io::Result<()> {
    // App::with_parent() scans the parent dir for projects and discovers live
    // sessions, defaulting to MockRuntime — so there is no orchestrator wiring
    // to do here (cf. upstream `run_tui`, which swaps in a live brain/coord/bus
    // runtime — deliberately omitted).
    let mut app = App::with_parent(opts.parent);
    if opts.include_non_git {
        app.include_non_git = true;
        app.refresh(); // re-scan now that the widen flag is set
    }
    let mut last_tick = Instant::now();

    loop {
        terminal.draw(|frame| {
            let area = frame.area();
            // Skills overlay is the one full-screen panel we vendored. Upstream
            // also has a Brain Review screen, but its renderer lives in the
            // binary crate we didn't fork; opening it simply falls through to
            // the roster (Esc returns) rather than rendering a missing screen.
            if app.show_skills {
                ui::skills::render_skills_screen(frame, area, &app);
                return;
            }
            ui::table::render(frame, area, &app);
        })?;

        let timeout = TICK_RATE
            .checked_sub(last_tick.elapsed())
            .unwrap_or(Duration::ZERO);

        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if !app.handle_key(key) {
                    return Ok(());
                }
            }
        }

        if last_tick.elapsed() >= TICK_RATE {
            app.tick();
            last_tick = Instant::now();
        }
    }
}
