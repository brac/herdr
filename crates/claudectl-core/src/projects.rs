//! Project discovery — the project-first spine (CLAUDE.md §2).
//!
//! Scans the parent directory herdr was launched from for project directories.
//! Unlike session discovery (`discovery::scan_sessions`), a project exists here
//! whether or not it has any live agents — agents hang *off* projects, not the
//! other way around. This is the inversion Phase 1 is built on.

use std::fs;
use std::path::{Path, PathBuf};

/// A project directory under the parent herdr runs from.
///
/// Phase 1 carries identity only; git status (branch / dirty / ahead-behind) is
/// the Phase 3 light path, build state later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub path: PathBuf,
    pub name: String,
    pub has_git: bool,
}

/// Scan `parent` one level deep for project directories.
///
/// By default keeps only directories containing a `.git` entry (53 of 77 dirs
/// under a typical `~/Work`); `include_non_git` widens to every visible
/// subdirectory. Defensive: an unreadable parent yields an empty roster and
/// unreadable entries are skipped — never panics (CLAUDE.md §3). Sorted by name
/// for a stable roster.
pub fn scan(parent: &Path, include_non_git: bool) -> Vec<Project> {
    let entries = match fs::read_dir(parent) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let mut projects: Vec<Project> = entries
        .flatten()
        .filter_map(|entry| {
            // `file_type()` avoids a stat round-trip and doesn't follow symlinks;
            // fall back to `is_dir()` only when the type is unavailable.
            let is_dir = entry
                .file_type()
                .map(|t| t.is_dir())
                .unwrap_or_else(|_| entry.path().is_dir());
            if !is_dir {
                return None;
            }
            // Skip dot-directories (`.git`, `.cache`, …) as top-level projects.
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                return None;
            }
            let path = entry.path();
            let has_git = path.join(".git").exists();
            if !has_git && !include_non_git {
                return None;
            }
            Some(Project {
                path,
                name,
                has_git,
            })
        })
        .collect();

    projects.sort_by(|a, b| a.name.cmp(&b.name));
    projects
}

/// Whether `cwd` belongs to `project_path`: the project dir itself, or any path
/// nested under it (an agent running in a subdirectory or a linked worktree).
///
/// `Path::starts_with` is component-wise, so `/a/foo` does **not** match
/// `/a/foobar` — a plain string prefix test would. Callers should pass
/// already-resolved (canonicalized) paths; this is pure logic with no I/O.
pub fn contains_cwd(project_path: &Path, cwd: &Path) -> bool {
    cwd.starts_with(project_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn mkdir(root: &Path, rel: &str) -> PathBuf {
        let p = root.join(rel);
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn git_repo(root: &Path, rel: &str) -> PathBuf {
        let p = mkdir(root, rel);
        fs::create_dir_all(p.join(".git")).unwrap();
        p
    }

    #[test]
    fn scan_keeps_only_git_dirs_by_default() {
        let tmp = tempdir().unwrap();
        git_repo(tmp.path(), "alpha");
        git_repo(tmp.path(), "beta");
        mkdir(tmp.path(), "plain"); // no .git → excluded by default

        let found = scan(tmp.path(), false);
        let names: Vec<&str> = found.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "beta"]);
        assert!(found.iter().all(|p| p.has_git));
    }

    #[test]
    fn include_non_git_widens_to_all_visible_dirs() {
        let tmp = tempdir().unwrap();
        git_repo(tmp.path(), "alpha");
        mkdir(tmp.path(), "plain");

        let found = scan(tmp.path(), true);
        let names: Vec<&str> = found.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "plain"]);
        // has_git is still recorded accurately even when included.
        assert!(found.iter().find(|p| p.name == "alpha").unwrap().has_git);
        assert!(!found.iter().find(|p| p.name == "plain").unwrap().has_git);
    }

    #[test]
    fn skips_files_and_dot_dirs() {
        let tmp = tempdir().unwrap();
        git_repo(tmp.path(), "repo");
        fs::write(tmp.path().join("README.md"), "x").unwrap();
        git_repo(tmp.path(), ".dotrepo"); // dot-dir, even with .git → skipped

        let found = scan(tmp.path(), true);
        let names: Vec<&str> = found.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["repo"]);
    }

    #[test]
    fn scan_is_one_level_deep() {
        let tmp = tempdir().unwrap();
        // Nested repo: outer has no .git of its own, inner does.
        git_repo(tmp.path(), "outer/inner");

        // Default: `outer` lacks .git so it's excluded; `inner` is depth 2, not scanned.
        assert!(scan(tmp.path(), false).is_empty());
        // Widened: only the depth-1 `outer` appears, never `inner`.
        let found = scan(tmp.path(), true);
        let names: Vec<&str> = found.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["outer"]);
    }

    #[test]
    fn unreadable_parent_yields_empty() {
        let tmp = tempdir().unwrap();
        let missing = tmp.path().join("does-not-exist");
        assert!(scan(&missing, true).is_empty());
    }

    #[test]
    fn contains_cwd_matches_self_and_subdirs_componentwise() {
        let proj = Path::new("/Users/me/work/api");
        assert!(contains_cwd(proj, Path::new("/Users/me/work/api")));
        assert!(contains_cwd(proj, Path::new("/Users/me/work/api/src/lib")));
        // Shared string prefix but different component → not a match.
        assert!(!contains_cwd(proj, Path::new("/Users/me/work/api-v2")));
        assert!(!contains_cwd(proj, Path::new("/Users/me/work/web")));
    }
}
