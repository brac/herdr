use std::fs;
use std::path::{Path, PathBuf};

use crate::session::{ClaudeSession, RawSession};

fn sessions_dir() -> PathBuf {
    dirs_home().join(".claude").join("sessions")
}

fn dirs_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

pub fn projects_dir() -> PathBuf {
    dirs_home().join(".claude").join("projects")
}

/// Where the opt-in inbound-hook channel writes per-session status files
/// (`~/.claude/herdr/<session_id>.json`). See `crate::hookstate`.
pub fn herdr_state_dir() -> PathBuf {
    dirs_home().join(".claude").join("herdr")
}

pub fn scan_sessions() -> Vec<ClaudeSession> {
    let dir = sessions_dir();
    cleanup_stale_sessions(&dir);
    let entries = match fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let mut sessions = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }

        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                crate::logger::log(
                    "WARN",
                    &format!("session file read error: {}: {e}", path.display()),
                );
                continue;
            }
        };

        let raw: RawSession = match serde_json::from_str(&content) {
            Ok(r) => r,
            Err(e) => {
                crate::logger::log(
                    "WARN",
                    &format!("session file parse error: {}: {e}", path.display()),
                );
                continue;
            }
        };

        // JSONL path resolved later by resolve_jsonl_paths() after command_args are populated
        let mut session = ClaudeSession::from_raw(raw);
        // Claude Code stamps an auto-generated task `name` into the same marker; lift
        // it for display (defensive — older/partial markers may omit it).
        session.cc_name = serde_json::from_str::<serde_json::Value>(&content)
            .ok()
            .as_ref()
            .and_then(|v| v.get("name"))
            .and_then(|n| n.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        sessions.push(session);
    }

    sessions
}

/// Resolve JSONL paths for sessions. Must be called AFTER command_args are populated
/// (i.e., after fetch_ps_data), so we can use --resume UUIDs for correct mapping.
pub fn resolve_jsonl_paths(sessions: &mut [ClaudeSession]) {
    let base = projects_dir();
    for session in sessions.iter_mut() {
        resolve_session_jsonl(session, &base);
    }
}

/// Attach a session's transcript **by session id only** — never by guessing the
/// most-recently-modified file in the project dir. A freshly launched agent (or
/// one whose conversation was just reset with `/clear`) has no transcript named
/// for its id yet, so it stays `None` (telemetry Pending) until its own
/// `<session_id>.jsonl` appears — rather than inheriting the *previous* session's
/// transcript and showing its context/status (BACKLOG: status/context incorrect
/// on launch; context should reset on /clear).
fn resolve_session_jsonl(session: &mut ClaudeSession, projects_base: &Path) {
    let slug = cwd_to_slug(&session.cwd);
    let project_dir = projects_base.join(&slug);

    // Priority 1: the session's own id in the expected project dir.
    let own_path = project_dir.join(format!("{}.jsonl", session.session_id));
    if own_path.exists() {
        session.jsonl_path = Some(own_path);
        return;
    }

    // Priority 2: a --resume UUID from the command args.
    if let Some(resume_id) = extract_resume_uuid(&session.command_args) {
        let resume_path = project_dir.join(format!("{resume_id}.jsonl"));
        if resume_path.exists() {
            session.jsonl_path = Some(resume_path);
            return;
        }
    }

    // Priority 3: the session id in ANY project dir. Handles cwd-encoding/slug
    // mismatches between herdr and Claude Code (symlink resolution, path
    // normalization) — still an exact session-id match, never a guess.
    if let Some(found) = search_all_projects_for_session(projects_base, &session.session_id) {
        crate::logger::log(
            "DEBUG",
            &format!(
                "session {}: slug mismatch — found JSONL via project scan: {}",
                session.session_id,
                found.display()
            ),
        );
        session.jsonl_path = Some(found);
        return;
    }

    crate::logger::log(
        "DEBUG",
        &format!(
            "session {}: no JSONL found (slug={}, project_dir_exists={})",
            session.session_id,
            slug,
            project_dir.exists()
        ),
    );
}

/// Search all directories under `projects_base` for a JSONL file matching the
/// session ID. A fallback when the cwd-based slug doesn't match the actual
/// directory on disk.
fn search_all_projects_for_session(base: &Path, session_id: &str) -> Option<PathBuf> {
    let filename = format!("{session_id}.jsonl");
    let entries = fs::read_dir(base).ok()?;

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let candidate = path.join(&filename);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

/// Extract the UUID from a --resume argument in command args.
fn extract_resume_uuid(command_args: &str) -> Option<String> {
    let marker = "--resume ";
    let start = command_args.find(marker)? + marker.len();
    let rest = &command_args[start..];
    // Take until whitespace — could be a UUID or a named session
    let token: String = rest.chars().take_while(|c| !c.is_whitespace()).collect();
    if token.is_empty() {
        return None;
    }
    // Strip surrounding quotes
    let token = token.trim_matches('"').trim_matches('\'');
    Some(token.to_string())
}

/// A subagent transcript is "active" if its JSONL was written within this many
/// seconds — fresh writes mean the sub-agent is still streaming. Older ones have
/// finished but their files (and token spend) persist on disk, so they still roll
/// up into the parent; they just collapse into the "completed (N)" row.
pub const SUBAGENT_ACTIVE_SECS: u64 = 25;

/// Scan for a session's sub-agent transcripts (Task tool / workflow agents).
///
/// Claude Code (≥ v2.1.x) writes each sub-agent to its own file under the parent
/// session's transcript dir:
///   `~/.claude/projects/{slug}/{sessionId}/subagents/agent-*.jsonl`
///   `~/.claude/projects/{slug}/{sessionId}/subagents/workflows/wf_*/agent-*.jsonl`
/// (the old `/tmp/claude-{uid}/{slug}/{sessionId}/tasks/` path holds only `.output`
/// scratch files — never the `.jsonl` we need, which is why sub-agents used to be
/// invisible). We derive the dir from `jsonl_path` so resumed/relocated sessions
/// resolve correctly, then split discovered files into *all* (rollup) and the
/// recently-written *active* subset (live rows).
pub fn scan_subagents(sessions: &mut [ClaudeSession]) {
    let now = std::time::SystemTime::now();

    for session in sessions.iter_mut() {
        let Some(subagents_dir) = subagents_dir_for(session) else {
            session.active_subagent_count = 0;
            session.active_subagent_jsonl_paths.clear();
            session.subagent_jsonl_paths.clear();
            continue;
        };

        if !subagents_dir.exists() {
            session.active_subagent_count = 0;
            session.active_subagent_jsonl_paths.clear();
            session.subagent_jsonl_paths.clear();
            continue;
        }

        let mut jsonls = Vec::new();
        collect_subagent_jsonls(&subagents_dir, &mut jsonls);
        jsonls.sort();

        let active: Vec<PathBuf> = jsonls
            .iter()
            .filter(|p| is_recently_modified(p, now, SUBAGENT_ACTIVE_SECS))
            .cloned()
            .collect();

        session.active_subagent_count = active.len();
        session.active_subagent_jsonl_paths = active;
        session.subagent_jsonl_paths = jsonls;
    }
}

/// The `subagents/` directory for a session, derived from its resolved transcript
/// path (`{sessionId}.jsonl` → `{sessionId}/subagents`). Falls back to the
/// slug+id path under `projects/` when the transcript hasn't resolved yet.
fn subagents_dir_for(session: &ClaudeSession) -> Option<PathBuf> {
    if let Some(jsonl) = &session.jsonl_path {
        let session_dir = jsonl.with_extension("");
        return Some(session_dir.join("subagents"));
    }
    if session.session_id.is_empty() {
        return None;
    }
    let slug = cwd_to_slug(&session.cwd);
    Some(
        projects_dir()
            .join(slug)
            .join(&session.session_id)
            .join("subagents"),
    )
}

/// True when `path`'s mtime is within `window_secs` of `now`.
fn is_recently_modified(path: &Path, now: std::time::SystemTime, window_secs: u64) -> bool {
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return false;
    };
    now.duration_since(modified)
        .map(|age| age.as_secs() <= window_secs)
        .unwrap_or(true) // mtime in the future (clock skew) → treat as fresh
}

fn collect_subagent_jsonls(dir: &Path, jsonls: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_subagent_jsonls(&path, jsonls);
            continue;
        }
        // `agent-*.jsonl` only — skip the `agent-*.meta.json` sidecars.
        let is_agent_jsonl = path.extension().and_then(|e| e.to_str()) == Some("jsonl")
            && path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("agent-"));
        if is_agent_jsonl {
            jsonls.push(path);
        }
    }
}

/// Resolve git worktree identity for each session (for conflict detection).
/// Sessions in different worktrees of the same repo get different IDs.
/// Runs `git rev-parse --show-toplevel` once per unique cwd.
pub fn resolve_worktree_ids(sessions: &mut [ClaudeSession]) {
    // Cache results to avoid running git multiple times for the same cwd
    let mut cache: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    for session in sessions.iter_mut() {
        if session.worktree_id.is_some() {
            continue;
        }
        let id = if let Some(cached) = cache.get(&session.cwd) {
            cached.clone()
        } else {
            let resolved = std::process::Command::new("git")
                .args(["rev-parse", "--show-toplevel"])
                .current_dir(&session.cwd)
                .output()
                .ok()
                .and_then(|o| {
                    if o.status.success() {
                        String::from_utf8(o.stdout)
                            .ok()
                            .map(|s| s.trim().to_string())
                    } else {
                        None
                    }
                })
                // Fall back to cwd if not a git repo
                .unwrap_or_else(|| session.cwd.clone());
            cache.insert(session.cwd.clone(), resolved.clone());
            resolved
        };
        session.worktree_id = Some(id);
    }
}

/// Map a working directory to Claude Code's transcript-dir slug under
/// `~/.claude/projects/`. Claude Code replaces **every non-alphanumeric
/// character** (path separators, spaces, dots, underscores, …) with `-`,
/// preserving case — e.g. `/mnt/c/Users/Ben Bracamonte/Work/burnRat`
/// → `-mnt-c-Users-Ben-Bracamonte-Work-burnRat`.
///
/// A previous version replaced only `/`, so any project path containing a
/// space or dot computed the wrong slug and silently failed to match its
/// transcript dir (verified on a `…/Ben Bracamonte/…` machine). Mirror the
/// real rule (confirmed against live `~/.claude/projects/` dirs and
/// agent-deck's `ConvertToClaudeDirName`); never re-derive it loosely (§8).
fn cwd_to_slug(cwd: &str) -> String {
    let trimmed = cwd.trim_end_matches('/');
    if trimmed.is_empty() {
        return "-".to_string();
    }
    trimmed
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Remove session JSON files for dead PIDs whose files are older than 24 hours.
/// This prevents stale files from previous runs accumulating in ~/.claude/sessions/.
fn cleanup_stale_sessions(dir: &std::path::Path) {
    const MAX_AGE: std::time::Duration = std::time::Duration::from_secs(24 * 3600);
    let now = std::time::SystemTime::now();

    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(pid) = stem.parse::<u32>() else {
            continue;
        };

        if pid_alive(pid) {
            continue;
        }

        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        let Ok(age) = now.duration_since(modified) else {
            continue;
        };

        if age > MAX_AGE {
            crate::logger::log(
                "DEBUG",
                &format!(
                    "cleaning stale session file: {} (PID {pid})",
                    path.display()
                ),
            );
            let _ = fs::remove_file(&path);
        }
    }
}

fn pid_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn touch_jsonl(dir: &Path, stem: &str) -> PathBuf {
        let path = dir.join(format!("{stem}.jsonl"));
        std::fs::File::create(&path)
            .unwrap()
            .write_all(b"{}\n")
            .unwrap();
        path
    }

    #[test]
    fn resolves_transcript_by_session_id_in_its_slug_dir() {
        let base = tempfile::tempdir().unwrap();
        let cwd = "/home/u/proj";
        let dir = base.path().join(cwd_to_slug(cwd));
        std::fs::create_dir_all(&dir).unwrap();
        let id = "11111111-1111-1111-1111-111111111111";
        let own = touch_jsonl(&dir, id);
        // A newer, foreign transcript that the old "latest jsonl" heuristic would
        // have wrongly grabbed.
        touch_jsonl(&dir, "22222222-2222-2222-2222-222222222222");

        let mut s = ClaudeSession::from_raw(RawSession {
            pid: 1,
            session_id: id.into(),
            cwd: cwd.into(),
            started_at: 0,
        });
        resolve_session_jsonl(&mut s, base.path());
        assert_eq!(s.jsonl_path.as_deref(), Some(own.as_path()));
    }

    #[test]
    fn fresh_session_does_not_inherit_a_foreign_transcript() {
        // BACKLOG (status/context incorrect on launch; /clear reset): a session
        // whose own <id>.jsonl doesn't exist yet must stay unresolved, NOT adopt
        // the previous session's most-recent transcript.
        let base = tempfile::tempdir().unwrap();
        let cwd = "/home/u/proj";
        let dir = base.path().join(cwd_to_slug(cwd));
        std::fs::create_dir_all(&dir).unwrap();
        touch_jsonl(&dir, "99999999-9999-9999-9999-999999999999"); // a prior session

        let mut s = ClaudeSession::from_raw(RawSession {
            pid: 2,
            session_id: "fresh-session-with-no-file-yet".into(),
            cwd: cwd.into(),
            started_at: 0,
        });
        resolve_session_jsonl(&mut s, base.path());
        assert!(
            s.jsonl_path.is_none(),
            "a fresh agent must not inherit another session's transcript"
        );
    }

    #[test]
    fn subagents_dir_derives_from_resolved_transcript_path() {
        // `{sessionId}.jsonl` → `{sessionId}/subagents` next to the transcript.
        let mut s = ClaudeSession::from_raw(RawSession {
            pid: 1,
            session_id: "abc".into(),
            cwd: "/home/u/proj".into(),
            started_at: 0,
        });
        s.jsonl_path = Some(PathBuf::from("/x/-home-u-proj/abc.jsonl"));
        assert_eq!(
            subagents_dir_for(&s),
            Some(PathBuf::from("/x/-home-u-proj/abc/subagents"))
        );
    }

    #[test]
    fn scan_subagents_finds_agent_jsonls_and_splits_active_by_mtime() {
        // Lay out a real `…/{sessionId}/subagents/` dir with two agent transcripts
        // (one fresh, one stale) plus a `.meta.json` sidecar that must be ignored.
        let base = tempfile::tempdir().unwrap();
        let cwd = "/home/u/proj";
        let slug_dir = base.path().join(cwd_to_slug(cwd));
        let id = "33333333-3333-3333-3333-333333333333";
        std::fs::create_dir_all(&slug_dir).unwrap();
        let transcript = touch_jsonl(&slug_dir, id);

        let subagents = slug_dir.join(id).join("subagents");
        std::fs::create_dir_all(&subagents).unwrap();
        let fresh = touch_jsonl(&subagents, "agent-fresh");
        let stale = touch_jsonl(&subagents, "agent-stale");
        // A sidecar + a non-agent file that must NOT be collected.
        std::fs::File::create(subagents.join("agent-fresh.meta.json")).unwrap();
        std::fs::File::create(subagents.join("notes.jsonl")).unwrap();

        // Age the stale one well past the active window.
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(SUBAGENT_ACTIVE_SECS + 600);
        std::fs::File::options()
            .write(true)
            .open(&stale)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(old))
            .unwrap();

        let mut s = ClaudeSession::from_raw(RawSession {
            pid: 1,
            session_id: id.into(),
            cwd: cwd.into(),
            started_at: 0,
        });
        s.jsonl_path = Some(transcript);

        scan_subagents(std::slice::from_mut(&mut s));

        // All agent-*.jsonl discovered (sidecar + notes.jsonl excluded), fresh one active.
        assert_eq!(s.subagent_jsonl_paths.len(), 2, "both agent transcripts discovered");
        assert!(s.subagent_jsonl_paths.contains(&fresh));
        assert!(s.subagent_jsonl_paths.contains(&stale));
        assert_eq!(s.active_subagent_jsonl_paths, vec![fresh], "only the fresh one is active");
        assert_eq!(s.active_subagent_count, 1);
    }

    #[test]
    fn scan_subagents_clears_when_no_subagents_dir() {
        let base = tempfile::tempdir().unwrap();
        let slug_dir = base.path().join(cwd_to_slug("/home/u/proj"));
        std::fs::create_dir_all(&slug_dir).unwrap();
        let transcript = touch_jsonl(&slug_dir, "nosub");

        let mut s = ClaudeSession::from_raw(RawSession {
            pid: 1,
            session_id: "nosub".into(),
            cwd: "/home/u/proj".into(),
            started_at: 0,
        });
        s.jsonl_path = Some(transcript);
        // Pre-seed stale state to prove it gets cleared.
        s.subagent_jsonl_paths = vec![PathBuf::from("/old")];
        s.active_subagent_jsonl_paths = vec![PathBuf::from("/old")];
        s.active_subagent_count = 1;

        scan_subagents(std::slice::from_mut(&mut s));
        assert!(s.subagent_jsonl_paths.is_empty());
        assert!(s.active_subagent_jsonl_paths.is_empty());
        assert_eq!(s.active_subagent_count, 0);
    }

    #[test]
    fn slug_basic_path() {
        assert_eq!(cwd_to_slug("/Users/foo/bar"), "-Users-foo-bar");
    }

    #[test]
    fn slug_trailing_slash() {
        // Must strip trailing slash — otherwise slug ends with "-" and won't match disk
        assert_eq!(
            cwd_to_slug("/Users/foo/bar/"),
            "-Users-foo-bar",
            "trailing slash must be stripped before slugifying"
        );
    }

    #[test]
    fn slug_multiple_trailing_slashes() {
        assert_eq!(cwd_to_slug("/Users/foo/bar///"), "-Users-foo-bar");
    }

    #[test]
    fn slug_with_hyphens_in_name() {
        assert_eq!(
            cwd_to_slug("/Users/dev/data-platform-answers"),
            "-Users-dev-data-platform-answers"
        );
    }

    #[test]
    fn slug_root() {
        assert_eq!(cwd_to_slug("/"), "-");
    }

    #[test]
    fn slug_single_component() {
        assert_eq!(cwd_to_slug("/tmp"), "-tmp");
    }

    #[test]
    fn slug_replaces_spaces() {
        // Regression: paths with spaces (e.g. Windows "Ben Bracamonte") must
        // hyphenate the space, matching Claude Code's real transcript dir.
        assert_eq!(
            cwd_to_slug("/mnt/c/Users/Ben Bracamonte/Work/herdr"),
            "-mnt-c-Users-Ben-Bracamonte-Work-herdr"
        );
    }

    #[test]
    fn slug_replaces_dots_and_underscores() {
        assert_eq!(
            cwd_to_slug("/Users/dev/brac.dev/my_app"),
            "-Users-dev-brac-dev-my-app"
        );
    }

    #[test]
    fn slug_preserves_case() {
        // burnRat keeps its capital R on disk — no lowercasing.
        assert_eq!(cwd_to_slug("/Work/burnRat"), "-Work-burnRat");
    }
}
