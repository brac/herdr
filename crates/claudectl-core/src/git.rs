//! Phase 3 (CLAUDE.md §4): the git **light path**. Shell out to the `git`
//! binary for a per-project status glance — branch, dirty flag, ahead/behind —
//! and nothing more. No vendored git library, no diff/staging/rebase UI (§8).
//!
//! Defensive (§3): any failure (not a repo, `git` missing, unexpected output)
//! yields `None`; the parser ignores unknown lines so a future porcelain format
//! degrades gracefully rather than crashing.

use std::path::Path;
use std::process::Command;

/// A light git status glance for one project, sourced from a single
/// `git status --porcelain=v2 --branch` invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitStatus {
    /// Branch name; `(detached)` when HEAD is detached, `(bare)` for a bare
    /// repo, or `(unknown)` if the branch header was absent.
    pub branch: String,
    /// Any staged, unstaged, untracked, or unmerged change is present.
    pub dirty: bool,
    /// Commits ahead of upstream (0 when there is no upstream).
    pub ahead: u32,
    /// Commits behind upstream (0 when there is no upstream).
    pub behind: u32,
    /// Whether an upstream is configured. When false, `ahead`/`behind` are
    /// meaningless and the renderer should omit them.
    pub upstream: bool,
    /// A bare repository (no working tree) — dirty/ahead-behind don't apply.
    pub bare: bool,
}

impl Default for GitStatus {
    fn default() -> Self {
        Self {
            branch: "(unknown)".to_string(),
            dirty: false,
            ahead: 0,
            behind: 0,
            upstream: false,
            bare: false,
        }
    }
}

/// Light git status for `path`, or `None` if it isn't a git working tree or
/// `git` is unavailable. One `git status --porcelain=v2 --branch` spawn in the
/// common case; a second `rev-parse` only when status fails, to tell a bare
/// repo apart from "not a repo at all" (agent-deck's `isBareRepoSelf` idea).
pub fn status(path: &Path) -> Option<GitStatus> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["status", "--porcelain=v2", "--branch"])
        .output()
        .ok()?;

    if output.status.success() {
        return Some(parse_porcelain_v2(&String::from_utf8_lossy(&output.stdout)));
    }

    // `status` fails in a bare repo ("must be run in a work tree"). Distinguish
    // that from a non-repo so we can render `(bare)` rather than nothing.
    if is_bare_repo(path) {
        return Some(GitStatus {
            branch: "(bare)".to_string(),
            bare: true,
            ..Default::default()
        });
    }

    None
}

/// Whether `path` is a bare repository.
fn is_bare_repo(path: &Path) -> bool {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--is-bare-repository"])
        .output();
    matches!(output, Ok(o)
        if o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "true")
}

/// Parse `git status --porcelain=v2 --branch` stdout. Pure — the unit-testable
/// core. Header lines are `# branch.*`; entry lines start with `1`/`2` (changed
/// or renamed), `u` (unmerged), or `?` (untracked). `!` (ignored) only appears
/// with `--ignored`, which we don't pass. Unknown lines are ignored (§3).
fn parse_porcelain_v2(stdout: &str) -> GitStatus {
    let mut st = GitStatus::default();

    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("# branch.head ") {
            st.branch = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("# branch.ab ") {
            // Presence of this header means an upstream is configured.
            st.upstream = true;
            for tok in rest.split_whitespace() {
                if let Some(n) = tok.strip_prefix('+') {
                    st.ahead = n.parse().unwrap_or(0);
                } else if let Some(n) = tok.strip_prefix('-') {
                    st.behind = n.parse().unwrap_or(0);
                }
            }
        } else if is_changed_entry(line) {
            st.dirty = true;
        }
    }

    st
}

/// A porcelain-v2 entry line indicating the working tree differs: a changed
/// (`1`), renamed/copied (`2`), unmerged (`u`), or untracked (`?`) entry. Each
/// is a single-char field followed by a space, which also rules out the `#`
/// header lines.
fn is_changed_entry(line: &str) -> bool {
    matches!(line.as_bytes(), [b'1' | b'2' | b'u' | b'?', b' ', ..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_branch_with_upstream() {
        let out = "# branch.oid abc123\n# branch.head main\n# branch.upstream origin/main\n# branch.ab +0 -0\n";
        let st = parse_porcelain_v2(out);
        assert_eq!(st.branch, "main");
        assert!(!st.dirty);
        assert!(st.upstream);
        assert_eq!((st.ahead, st.behind), (0, 0));
    }

    #[test]
    fn ahead_and_behind_parsed() {
        let st = parse_porcelain_v2("# branch.head main\n# branch.ab +2 -3\n");
        assert!(st.upstream);
        assert_eq!((st.ahead, st.behind), (2, 3));
    }

    #[test]
    fn no_upstream_means_no_ab_line() {
        // A branch with no upstream emits no `branch.ab` line at all.
        let st = parse_porcelain_v2("# branch.oid abc\n# branch.head feature\n");
        assert_eq!(st.branch, "feature");
        assert!(!st.upstream);
        assert_eq!((st.ahead, st.behind), (0, 0));
    }

    #[test]
    fn detached_head() {
        let st = parse_porcelain_v2("# branch.oid abc123\n# branch.head (detached)\n");
        assert_eq!(st.branch, "(detached)");
    }

    #[test]
    fn changed_entry_is_dirty() {
        let st = parse_porcelain_v2("# branch.head main\n1 .M N... 100644 100644 100644 aaa bbb src/lib.rs\n");
        assert!(st.dirty);
    }

    #[test]
    fn renamed_entry_is_dirty() {
        let st = parse_porcelain_v2("# branch.head main\n2 R. N... 100644 100644 100644 aaa bbb R100 new.rs\told.rs\n");
        assert!(st.dirty);
    }

    #[test]
    fn unmerged_entry_is_dirty() {
        let st = parse_porcelain_v2("# branch.head main\nu UU N... 100644 100644 100644 100644 aaa bbb ccc conflict.rs\n");
        assert!(st.dirty);
    }

    #[test]
    fn untracked_entry_is_dirty() {
        let st = parse_porcelain_v2("# branch.head main\n? newfile.txt\n");
        assert!(st.dirty);
    }

    #[test]
    fn empty_or_garbage_yields_defaults() {
        let st = parse_porcelain_v2("");
        assert_eq!(st.branch, "(unknown)");
        assert!(!st.dirty);
        assert!(!st.upstream);

        // Unknown lines must not crash or flip dirty.
        let st2 = parse_porcelain_v2("totally unexpected\n# branch.weird\n");
        assert!(!st2.dirty);
        assert_eq!(st2.branch, "(unknown)");
    }

    #[test]
    fn header_lines_are_not_dirty() {
        // The `#` headers must never be mistaken for changed entries.
        let st = parse_porcelain_v2("# branch.oid abc\n# branch.head main\n# branch.upstream origin/main\n# branch.ab +1 -0\n");
        assert!(!st.dirty);
    }

    /// Integration: run against a real `git init`ed tempdir. Skipped (not failed)
    /// when `git` isn't on PATH so CI without git doesn't break.
    #[test]
    fn status_reads_a_real_repo() {
        use std::fs;
        if Command::new("git").arg("--version").output().is_err() {
            return; // git unavailable — skip
        }
        let tmp = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(tmp.path())
                .args(args)
                .output()
                .unwrap()
        };
        git(&["init"]);
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "t"]);

        // Untracked file → dirty; branch header should be populated.
        fs::write(tmp.path().join("f.txt"), "x").unwrap();
        let st = status(tmp.path()).expect("a git repo should report status");
        assert!(st.dirty, "untracked file should mark the repo dirty");
        assert_ne!(st.branch, "(unknown)", "branch header should be parsed");
        assert!(!st.bare);

        // A non-repo dir yields None.
        let plain = tempfile::tempdir().unwrap();
        assert!(status(plain.path()).is_none(), "non-repo should be None");
    }
}
