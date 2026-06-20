//! Inbound Claude Code hook channel (Phase B, `docs/COMPARABLES.md` §7 item 5).
//!
//! herdr's status inference *guesses* when an agent is blocked on a permission
//! prompt (the invisible `<2% CPU + stale tool_use` heuristic). Claude Code's
//! `Notification` hook fires *exactly* when the agent needs permission or input,
//! so an opt-in hook (installed into `~/.claude/settings.json` by `herdr hook
//! install`) runs `herdr hook notify`, which writes a tiny per-session status file
//! here. herdr's `notify` watcher sees the write and the monitor reads it,
//! promoting the heuristic from a guess to a fact — zero new deps, zero polling,
//! fully inside the sync model (the hook is a separate short-lived process; herdr
//! only *reads* the file it leaves behind).
//!
//! Defensive throughout: a missing/malformed file is `None`, never an error, so it
//! can only ever *improve* status inference, never break it.

use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::discovery;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookStatus {
    /// `Notification` — the agent is waiting on the user (permission or input).
    NeedsInput,
    /// `Stop` / `SubagentStop` — the agent finished its turn.
    JobDone,
}

#[derive(Debug, Clone)]
pub struct HookState {
    pub status: HookStatus,
    /// Epoch-ms when the hook fired. Compared against the session's JSONL mtime so
    /// a stale state superseded by newer transcript activity is ignored.
    pub ts_ms: u64,
    pub message: String,
}

/// `~/.claude/herdr/`.
pub fn dir() -> PathBuf {
    discovery::herdr_state_dir()
}

/// Sanitize a Claude Code session id (a UUID) into a safe single-segment filename.
fn file_stem(session_id: &str) -> String {
    session_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' })
        .collect()
}

fn state_path(session_id: &str) -> PathBuf {
    dir().join(format!("{}.json", file_stem(session_id)))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Read the current hook state for a session, if any. Returns `None` on a
/// missing/unreadable/malformed file — never blocks status inference.
pub fn read(session_id: &str) -> Option<HookState> {
    let raw = std::fs::read_to_string(state_path(session_id)).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    let status = match v.get("status").and_then(|s| s.as_str())? {
        "NeedsInput" => HookStatus::NeedsInput,
        "JobDone" => HookStatus::JobDone,
        _ => return None,
    };
    Some(HookState {
        status,
        ts_ms: v.get("ts_ms").and_then(|t| t.as_u64()).unwrap_or(0),
        message: v
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string(),
    })
}

/// Handle `herdr hook notify`: read a Claude Code hook payload from **stdin** and
/// write the per-session status file atomically (tmp + rename, so the watcher never
/// reads a half-written file). Maps `Notification` → NeedsInput and `Stop`/
/// `SubagentStop` → JobDone. Always returns `Ok` on benign no-ops (no session id,
/// unrecognized event) so the hook never makes Claude Code report a failure.
pub fn write_from_stdin() -> io::Result<()> {
    let mut buf = String::new();
    io::stdin().read_to_string(&mut buf)?;
    let Ok(v) = serde_json::from_str::<Value>(&buf) else {
        return Ok(());
    };
    let Some(session_id) = v.get("session_id").and_then(|s| s.as_str()) else {
        return Ok(());
    };
    let event = v.get("hook_event_name").and_then(|s| s.as_str()).unwrap_or("");
    let status = match event {
        "Notification" => "NeedsInput",
        "Stop" | "SubagentStop" => "JobDone",
        _ => return Ok(()),
    };

    let dir = dir();
    std::fs::create_dir_all(&dir)?;
    let out = serde_json::json!({
        "status": status,
        "ts_ms": now_ms(),
        "event": event,
        "message": v.get("message").and_then(|m| m.as_str()).unwrap_or(""),
        "cwd": v.get("cwd").and_then(|c| c.as_str()).unwrap_or(""),
        "session_id": session_id,
    });

    let path = state_path(session_id);
    let tmp = dir.join(format!("{}.json.tmp", file_stem(session_id)));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(out.to_string().as_bytes())?;
        let _ = f.sync_all();
    }
    std::fs::rename(&tmp, &path)
}

// ── settings.json installer (opt-in, merge-safe) ────────────────────────────
//
// `herdr hook install` registers the `Notification` and `Stop` events so Claude
// Code runs `herdr hook notify` and feeds this channel. We merge into the user's
// existing `~/.claude/settings.json` rather than clobber it, and we tag our entries
// by a command suffix so install is idempotent and uninstall is precise.

/// The Claude Code events herdr subscribes to. `Notification` is the load-bearing
/// one (fires when the agent needs permission/input); `Stop` confirms turn end.
const EVENTS: [&str; 2] = ["Notification", "Stop"];

/// `~/.claude/settings.json`.
pub fn settings_path() -> PathBuf {
    discovery::herdr_state_dir()
        .parent()
        .map(|p| p.join("settings.json"))
        .unwrap_or_else(|| PathBuf::from("settings.json"))
}

/// Our hook entries end with this so they can be found again without depending on
/// the exact absolute binary path.
fn is_ours(cmd: &str) -> bool {
    cmd.trim_end().ends_with("hook notify")
}

fn group_is_ours(group: &Value) -> bool {
    group
        .get("hooks")
        .and_then(|h| h.as_array())
        .map(|hs| {
            hs.iter().any(|h| {
                h.get("command")
                    .and_then(|c| c.as_str())
                    .map(is_ours)
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

fn load_settings() -> Value {
    std::fs::read_to_string(settings_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}))
}

fn save_settings(v: &Value) -> io::Result<()> {
    let path = settings_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.herdr-tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(serde_json::to_string_pretty(v).unwrap_or_default().as_bytes())?;
        f.write_all(b"\n")?;
        let _ = f.sync_all();
    }
    std::fs::rename(&tmp, &path)
}

/// Merge the Notification/Stop hooks running `hook_command` into settings.json.
/// Idempotent (a prior herdr entry is replaced, not duplicated) and merge-safe
/// (other events and the user's own hooks for these events are preserved). Errors
/// if settings.json exists but isn't a JSON object, rather than clobbering it.
pub fn install(hook_command: &str) -> io::Result<()> {
    let mut settings = load_settings();
    let obj = settings.as_object_mut().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "~/.claude/settings.json is not a JSON object; refusing to overwrite",
        )
    })?;
    let hooks = obj
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}));
    let hooks = hooks.as_object_mut().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "settings.json `hooks` is not an object; refusing to overwrite",
        )
    })?;

    for event in EVENTS {
        let arr = hooks.entry(event).or_insert_with(|| serde_json::json!([]));
        let Some(arr) = arr.as_array_mut() else {
            continue;
        };
        arr.retain(|group| !group_is_ours(group));
        arr.push(serde_json::json!({
            "hooks": [ { "type": "command", "command": hook_command } ]
        }));
    }

    save_settings(&settings)
}

/// Remove herdr's hook entries from settings.json, dropping any event arrays that
/// become empty. Returns the number of entries removed. Idempotent.
pub fn uninstall() -> io::Result<usize> {
    let mut settings = load_settings();
    let Some(obj) = settings.as_object_mut() else {
        return Ok(0);
    };
    let Some(hooks) = obj.get_mut("hooks").and_then(|h| h.as_object_mut()) else {
        return Ok(0);
    };

    let mut removed = 0usize;
    for event in EVENTS {
        if let Some(arr) = hooks.get_mut(event).and_then(|a| a.as_array_mut()) {
            let before = arr.len();
            arr.retain(|group| !group_is_ours(group));
            removed += before - arr.len();
        }
    }
    for event in EVENTS {
        let empty = hooks
            .get(event)
            .and_then(|a| a.as_array())
            .map(|a| a.is_empty())
            .unwrap_or(false);
        if empty {
            hooks.remove(event);
        }
    }
    save_settings(&settings)?;
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_missing_is_none() {
        assert!(read("does-not-exist-zzz").is_none());
    }

    // All HOME-mutating assertions live in ONE test: env vars are process-global, so
    // two tests that both `set_var("HOME", …)` would race under the default parallel
    // runner. Running them sequentially in a single test avoids that.
    #[test]
    fn state_and_settings_under_temp_home() {
        let tmp = std::env::temp_dir().join(format!("herdr-hookstate-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let prev = std::env::var_os("HOME");
        // SAFETY: serialized within this single test; restored at the end.
        unsafe { std::env::set_var("HOME", &tmp) };

        // 1. read() round-trips a written state file.
        let d = dir();
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("sess-1.json"),
            br#"{"status":"NeedsInput","ts_ms":123,"message":"hi"}"#,
        )
        .unwrap();
        let s = read("sess-1").expect("should read back");
        assert_eq!(s.status, HookStatus::NeedsInput);
        assert_eq!(s.ts_ms, 123);
        assert_eq!(s.message, "hi");

        // 2. install merges (preserving a pre-existing unrelated hook), is
        //    idempotent, and uninstall removes only ours.
        let settings = settings_path();
        std::fs::write(
            &settings,
            r#"{"theme":"dark","hooks":{"Notification":[{"hooks":[{"type":"command","command":"someone-elses-thing"}]}]}}"#,
        )
        .unwrap();

        install("/abs/herdr hook notify").unwrap();
        install("/abs/herdr hook notify").unwrap(); // idempotent: no duplicate
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        assert_eq!(v["theme"], "dark", "unrelated keys preserved");
        let notif = v["hooks"]["Notification"].as_array().unwrap();
        assert_eq!(notif.len(), 2, "user's hook kept + exactly one herdr hook");
        assert_eq!(v["hooks"]["Stop"].as_array().unwrap().len(), 1);

        let removed = uninstall().unwrap();
        assert_eq!(removed, 2, "removed our Notification + Stop entries");
        let v2: Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        assert_eq!(
            v2["hooks"]["Notification"].as_array().unwrap().len(),
            1,
            "user's own Notification hook survives uninstall"
        );
        assert!(v2["hooks"].get("Stop").is_none(), "emptied Stop array dropped");

        // restore
        match prev {
            Some(v) => unsafe { std::env::set_var("HOME", v) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
