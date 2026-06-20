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
        sessions.push(ClaudeSession::from_raw(raw));
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

/// Feature #29: Scan for subagent task .jsonl files.
/// Claude Code spawns sub-agents whose files live in:
///   /tmp/claude-{uid}/{project_slug}/{sessionId}/tasks/
pub fn scan_subagents(sessions: &mut [ClaudeSession]) {
    let uid = unsafe { libc::getuid() };
    let tmp_base = PathBuf::from(format!("/tmp/claude-{uid}"));

    if !tmp_base.exists() {
        for session in sessions.iter_mut() {
            session.active_subagent_count = 0;
            session.active_subagent_jsonl_paths.clear();
        }
        return;
    }

    for session in sessions.iter_mut() {
        let slug = cwd_to_slug(&session.cwd);
        let tasks_dir = tmp_base.join(&slug).join(&session.session_id).join("tasks");

        if !tasks_dir.exists() {
            session.active_subagent_count = 0;
            session.active_subagent_jsonl_paths.clear();
            continue;
        }

        let mut jsonls = Vec::new();
        collect_subagent_jsonls(&tasks_dir, &mut jsonls);
        jsonls.sort();
        session.active_subagent_count = jsonls.len();
        session.active_subagent_jsonl_paths = jsonls;
    }
}

fn collect_subagent_jsonls(dir: &PathBuf, jsonls: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_subagent_jsonls(&path, jsonls);
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
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
