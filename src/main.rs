//! herdr — a project-first terminal cockpit for Claude Code agents.
//!
//! Phase 0 fork (CLAUDE.md §7). This binary drives claudectl's vendored data
//! layer (`claudectl-core`) and roster TUI (`claudectl-tui`) with a minimal
//! synchronous poll loop. Upstream's brain / bus / coord orchestrator is
//! intentionally absent: `App::new()` constructs an in-memory `MockRuntime`,
//! so every piece of live state comes from direct `~/.claude` session
//! discovery — no async runtime, no SQLite, no MCP server.
//!
//! The project-first inversion (CLAUDE.md §2) is Phase 1 and lands on top of
//! this; for now the roster is upstream's session-first table, proven against
//! live data to close the Phase 0 gate.

use std::io;
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
    install_panic_restore();
    let mut terminal = enter_tui()?;
    let result = run(&mut terminal);
    leave_tui(&mut terminal)?;
    result
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
fn run<W: io::Write>(terminal: &mut Terminal<CrosstermBackend<W>>) -> io::Result<()> {
    // App::new() discovers live sessions and defaults to MockRuntime, so there
    // is no orchestrator wiring to do here (cf. upstream `run_tui`, which swaps
    // in a live brain/coord/bus runtime — deliberately omitted).
    let mut app = App::new();
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
