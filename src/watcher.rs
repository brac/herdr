//! Phase 2.5 (CLAUDE.md §3, EVENT_LOOP_PLAN.md): event-driven refresh.
//!
//! A `notify` (fsnotify) watcher runs on its own thread and forwards each
//! relevant filesystem change as a unit `()` over an `mpsc` channel. The render
//! loop drains the channel each iteration and refreshes when anything arrived,
//! so an agent's JSONL activity repaints the roster without waiting for the
//! slow safety-net tick. Threads + channels only — **no tokio, no async** (§3).
//!
//! Failure is non-fatal: if the watcher can't initialize (permissions, missing
//! dir, unsupported platform) we return `None` and the caller falls back to
//! timed polling. A degraded watcher must never crash the TUI (§3 defensive).

use std::path::Path;
use std::sync::mpsc::{Receiver, channel};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

/// Spawn the watcher. Returns the receiver the loop drains plus the live
/// `Watcher` guard — **keep it alive**; dropping it stops watching.
///
/// - `claude_projects` (`~/.claude/projects`) is watched **recursively** so any
///   session's `<slug>/*.jsonl` write is seen, and new sessions are picked up.
/// - `parent_dir` (the project roster root) is watched **non-recursively** so a
///   newly created/removed project dir appears, without drowning in the file
///   churn of editors working *inside* those projects.
pub fn spawn(claude_projects: &Path, parent_dir: &Path) -> Option<(Receiver<()>, RecommendedWatcher)> {
    let (tx, rx) = channel::<()>();

    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res {
            if is_relevant(&event.kind) {
                // Ignore send errors: if the receiver is gone the loop has ended.
                let _ = tx.send(());
            }
        }
    })
    .ok()?;

    // A missing/!readable path is non-fatal — watch what we can, degrade on the
    // rest. If neither watch succeeds the caller still gets a (dead) receiver
    // and simply relies on the safety-net tick.
    let _ = watcher.watch(claude_projects, RecursiveMode::Recursive);
    let _ = watcher.watch(parent_dir, RecursiveMode::NonRecursive);

    Some((rx, watcher))
}

/// Forward only create/modify/remove. Drop `Access` (reads) and `Other`/`Any`
/// noise so we don't refresh on every transcript *read* — coalescing in the
/// loop collapses bursts, so a slightly loose filter just costs one extra
/// refresh, never a missed update.
fn is_relevant(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{CreateKind, EventKind, ModifyKind, RemoveKind};

    #[test]
    fn forwards_write_events() {
        assert!(is_relevant(&EventKind::Create(CreateKind::File)));
        assert!(is_relevant(&EventKind::Modify(ModifyKind::Any)));
        assert!(is_relevant(&EventKind::Remove(RemoveKind::File)));
    }

    #[test]
    fn drops_access_and_unknown_events() {
        use notify::event::AccessKind;
        assert!(!is_relevant(&EventKind::Access(AccessKind::Read)));
        assert!(!is_relevant(&EventKind::Any));
        assert!(!is_relevant(&EventKind::Other));
    }

    /// End-to-end: a file written under the watched dir delivers an event to the
    /// channel. Confirms `notify` actually fires on this platform (inotify on
    /// Linux/WSL). Runs against a tempdir on the Linux fs — note that inotify is
    /// unreliable on the WSL `/mnt/c` mount, which is exactly why the render loop
    /// keeps the safety-net tick as a fallback.
    #[test]
    fn delivers_event_for_a_real_write() {
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let (rx, _watcher) = spawn(dir.path(), dir.path()).expect("watcher should start");

        std::fs::write(dir.path().join("session.jsonl"), b"{}").unwrap();

        // Generous timeout: inotify delivery is near-instant but not guaranteed
        // synchronous. If this times out the watcher isn't delivering events.
        rx.recv_timeout(Duration::from_secs(5))
            .expect("a write under the watched dir should produce an event");
    }
}
