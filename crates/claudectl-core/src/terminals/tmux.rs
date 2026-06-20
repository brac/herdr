use crate::session::ClaudeSession;

pub fn launch(cwd: &str, prompt: Option<&str>, resume: Option<&str>) -> Result<String, String> {
    let mut parts = vec!["claude".to_string()];
    parts.extend(
        super::build_claude_args(prompt, resume)
            .into_iter()
            .map(|arg| super::shell_escape(&arg)),
    );
    let command = parts.join(" ");

    // Split herdr's pane (bottom half) so the agent opens as the *real* terminal
    // beside the overview — native rendering, talk to it directly — rather than
    // taking over a separate window. Target herdr's own pane via $TMUX_PANE so
    // the split lands here even if another pane is active. tmux owns the layout
    // from here (zoom with `prefix z`, rearrange as you like).
    // Split herdr's pane and print the new pane id so the caller can track it as
    // the single "staged" agent (see stage/unstage below).
    let mut args: Vec<String> = vec![
        "split-window".into(),
        "-v".into(),
        "-P".into(),
        "-F".into(),
        "#{pane_id}".into(),
    ];
    if let Ok(pane) = std::env::var("TMUX_PANE") {
        args.push("-t".into());
        args.push(pane);
    }
    args.push("-c".into());
    args.push(cwd.to_string());
    args.push(command);

    let output = std::process::Command::new("tmux")
        .args(&args)
        .output()
        .map_err(|e| format!("tmux split-window failed: {e}"))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

/// Resolve an agent's tmux pane id (`%N`) by matching its tty. Stable across
/// pane moves (the pty stays with the process), so it's safe to re-resolve.
pub fn pane_for_tty(tty: &str) -> Option<String> {
    let output = std::process::Command::new("tmux")
        .args(["list-panes", "-a", "-F", "#{pane_tty} #{pane_id}"])
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if let Some((pane_tty, pane_id)) = line.split_once(' ') {
            if pane_tty.contains(tty) {
                return Some(pane_id.to_string());
            }
        }
    }
    None
}

/// Total rows of herdr's tmux window (both split panes plus the divider). Used
/// to cap herdr's pane height so the staged agent below keeps a usable minimum.
/// `None` if not in tmux or the value can't be parsed. (Composing tmux — asking
/// for N rows — not tracking geometry ourselves; CLAUDE.md §8.)
pub fn window_height() -> Option<u16> {
    let pane = std::env::var("TMUX_PANE").ok()?;
    let output = std::process::Command::new("tmux")
        .args(["display-message", "-p", "-t", &pane, "#{window_height}"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

/// Move `pane` into herdr's window as the bottom split (the single agent
/// "stage"), keeping focus on herdr. Targets herdr's own pane via `$TMUX_PANE`.
pub fn join_into_herdr(pane: &str) -> Result<(), String> {
    let herdr = std::env::var("TMUX_PANE").map_err(|_| "herdr is not running inside tmux".to_string())?;
    if pane == herdr {
        return Err("that pane is herdr itself".into());
    }
    let output = std::process::Command::new("tmux")
        .args(["join-pane", "-v", "-l", "60%", "-s", pane, "-t", &herdr])
        .output()
        .map_err(|e| format!("tmux join-pane failed: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    // Keep driving from herdr; Ctrl-b ↓ to type into the agent.
    let _ = std::process::Command::new("tmux")
        .args(["select-pane", "-t", &herdr])
        .output();
    Ok(())
}

/// Window-scoped pane-border options herdr toggles around the stage. Listed once
/// so `clear_stage_border` can unset exactly what `set_stage_border` set.
const STAGE_BORDER_OPTS: &[&str] = &[
    "pane-border-status",
    "pane-border-format",
    "pane-border-lines",
    "pane-active-border-style",
    "pane-border-style",
];

/// Title the staged agent's pane and turn on titled pane borders, so the divider
/// under herdr reads like herdr's own `-herdr-` (BACKLOG "Claude window border").
/// Window-scoped tmux options targeting herdr's window — pure composition (§0.1),
/// best-effort. The agent is the bottom pane, so its top-edge border (the divider)
/// carries the title; herdr's pane is labelled "herdr" to match. tmux only draws
/// borders *between* panes, so the outer (left/right/bottom) edges stay at the
/// terminal edge — we make the divider read as a frame with a heavy line and a
/// Dracula-purple highlight on the focused pane's border.
pub fn set_stage_border(agent_pane: &str, title: &str) {
    let Ok(herdr) = std::env::var("TMUX_PANE") else {
        return;
    };
    let run = |args: &[&str]| {
        let _ = std::process::Command::new("tmux").args(args).output();
    };
    run(&["select-pane", "-t", agent_pane, "-T", title]);
    run(&["select-pane", "-t", &herdr, "-T", "herdr"]);
    run(&["set-option", "-w", "-t", &herdr, "pane-border-status", "top"]);
    run(&[
        "set-option",
        "-w",
        "-t",
        &herdr,
        "pane-border-format",
        " #{pane_title} ",
    ]);
    // Heavy line + Dracula border colors (purple = active/focused, comment = idle).
    run(&["set-option", "-w", "-t", &herdr, "pane-border-lines", "heavy"]);
    run(&[
        "set-option",
        "-w",
        "-t",
        &herdr,
        "pane-active-border-style",
        "fg=#bd93f9,bold",
    ]);
    run(&[
        "set-option",
        "-w",
        "-t",
        &herdr,
        "pane-border-style",
        "fg=#6272a4",
    ]);
}

/// Turn the staged-pane borders back off (nothing staged), unsetting every option
/// `set_stage_border` set so the window reverts to tmux defaults. Best-effort.
pub fn clear_stage_border() {
    let Ok(herdr) = std::env::var("TMUX_PANE") else {
        return;
    };
    for opt in STAGE_BORDER_OPTS {
        let _ = std::process::Command::new("tmux")
            .args(["set-option", "-w", "-u", "-t", &herdr, opt])
            .output();
    }
}

/// Break `pane` out of herdr's window back to its own background window, so it's
/// preserved but no longer visible (only one agent staged at a time).
pub fn break_out(pane: &str) -> Result<(), String> {
    let output = std::process::Command::new("tmux")
        .args(["break-pane", "-d", "-s", pane])
        .output()
        .map_err(|e| format!("tmux break-pane failed: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

pub fn switch(session: &ClaudeSession) -> Result<(), String> {
    // tmux can list panes with their TTY: `tmux list-panes -a -F '#{pane_tty} #{session_name}:#{window_index}.#{pane_index}'`
    let output = std::process::Command::new("tmux")
        .args([
            "list-panes",
            "-a",
            "-F",
            "#{pane_tty} #{session_name}:#{window_index}.#{pane_index}",
        ])
        .output()
        .map_err(|e| format!("tmux list-panes failed: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout);

    for line in stdout.lines() {
        let parts: Vec<&str> = line.splitn(2, ' ').collect();
        if parts.len() == 2 && parts[0].contains(&session.tty) {
            let target = parts[1]; // e.g. "main:2.1"
            // Select the tmux window+pane
            let _ = std::process::Command::new("tmux")
                .args(["select-window", "-t", target])
                .output();
            let _ = std::process::Command::new("tmux")
                .args(["select-pane", "-t", target])
                .output();
            return Ok(());
        }
    }

    Err(format!("TTY {} not found in tmux panes", session.tty))
}

pub fn send_input(session: &ClaudeSession, text: &str) -> Result<(), String> {
    let target = pane_target_for_tty(&session.tty).ok_or("TTY not found in tmux")?;
    let _ = std::process::Command::new("tmux")
        .args(["send-keys", "-t", &target, text, ""])
        .output();
    Ok(())
}

/// Resolve the `session:window.pane` target string for an agent's tty — the form
/// `send-keys`/`capture-pane` accept. `None` when the tty isn't found in any
/// tmux pane.
fn pane_target_for_tty(tty: &str) -> Option<String> {
    let output = std::process::Command::new("tmux")
        .args([
            "list-panes",
            "-a",
            "-F",
            "#{pane_tty} #{session_name}:#{window_index}.#{pane_index}",
        ])
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if let Some((pane_tty, target)) = line.split_once(' ') {
            if pane_tty.contains(tty) {
                return Some(target.to_string());
            }
        }
    }
    None
}

/// Send a tmux *named key* (e.g. `Escape`) to the agent's pane. Distinct from
/// [`send_input`], which sends literal text: tmux interprets the bare key name,
/// so this is how Phase 4c decline/interrupt deliver an Escape keystroke.
pub fn send_key(session: &ClaudeSession, key: &str) -> Result<(), String> {
    let target = pane_target_for_tty(&session.tty).ok_or("TTY not found in tmux")?;
    let output = std::process::Command::new("tmux")
        .args(["send-keys", "-t", &target, key])
        .output()
        .map_err(|e| format!("tmux send-keys failed: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

/// Capture the visible text of the agent's pane — the *rendered* terminal, which
/// is where Claude Code's permission dialog lives (it never reaches the JSONL).
/// Read-only scraping for a preview (CLAUDE.md §0.1, §8), not a terminal
/// emulator. `None` if the pane can't be found or tmux fails.
pub fn capture_pane(session: &ClaudeSession) -> Option<String> {
    let target = pane_target_for_tty(&session.tty)?;
    let output = std::process::Command::new("tmux")
        .args(["capture-pane", "-p", "-t", &target])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}
