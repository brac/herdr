//! `herdr hook …` — the inbound-hook CLI (Phase B, `docs/COMPARABLES.md` §7 item 5).
//!
//! Runs before the TUI and exits; never enters the render loop. The heavy lifting
//! (status-file format, settings.json merge) lives in `claudectl_core::hookstate` so
//! this stays a thin dispatcher and the binary needs no extra dependencies.
//!
//!   herdr hook notify     read a Claude Code hook payload on stdin → status file
//!   herdr hook install    merge opt-in Notification/Stop hooks into settings.json
//!   herdr hook uninstall  remove them again
//!   herdr hook status     print the current per-session hook state (debug)

use claudectl_core::hookstate;

/// Dispatch a `hook` subcommand. Returns the process exit code.
pub fn run(sub: Option<&str>) -> i32 {
    match sub {
        Some("notify") => match hookstate::write_from_stdin() {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("herdr hook notify: {e}");
                1
            }
        },
        Some("install") => {
            let cmd = hook_command();
            match hookstate::install(&cmd) {
                Ok(()) => {
                    println!(
                        "Installed herdr Notification/Stop hooks \u{2192} {}",
                        hookstate::settings_path().display()
                    );
                    println!("  command: {cmd}");
                    println!(
                        "Restart any running Claude Code sessions for the hooks to take effect."
                    );
                    0
                }
                Err(e) => {
                    eprintln!("install failed: {e}");
                    1
                }
            }
        }
        Some("uninstall") => match hookstate::uninstall() {
            Ok(n) => {
                println!(
                    "Removed {n} herdr hook entr{} from {}",
                    if n == 1 { "y" } else { "ies" },
                    hookstate::settings_path().display()
                );
                0
            }
            Err(e) => {
                eprintln!("uninstall failed: {e}");
                1
            }
        },
        Some("status") => status(),
        _ => {
            eprintln!("usage: herdr hook <notify|install|uninstall|status>");
            2
        }
    }
}

/// Absolute path to this binary + the subcommand, so the registered hook works
/// regardless of PATH and stays valid across rebuilds.
///
/// Claude Code runs hook commands through `/bin/sh -c`, so the binary path must be
/// shell-quoted — otherwise a path with spaces (e.g. `/mnt/c/Users/Ben Bracamonte/…`)
/// is word-split and `sh` reports `/mnt/c/Users/Ben: not found`. The `hook notify`
/// suffix stays bare so `hookstate::is_ours` still recognizes (and on re-install
/// replaces) both this and any older unquoted entry.
fn hook_command() -> String {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(String::from))
        .unwrap_or_else(|| "herdr".into());
    format!("{} hook notify", shell_single_quote(&exe))
}

/// Wrap a string in POSIX single quotes, escaping any embedded single quote as the
/// canonical `'\''`. Safe against spaces and all shell metacharacters for `sh -c`.
fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn status() -> i32 {
    let dir = hookstate::dir();
    println!("hook state dir: {}", dir.display());
    match std::fs::read_dir(&dir) {
        Ok(entries) => {
            let mut any = false;
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|x| x.to_str()) != Some("json") {
                    continue;
                }
                if let Some(name) = path.file_stem().and_then(|s| s.to_str())
                    && let Some(st) = hookstate::read(name)
                {
                    any = true;
                    println!("  {name}: {:?} ts_ms={} {}", st.status, st.ts_ms, st.message);
                }
            }
            if !any {
                println!("  (no status files)");
            }
        }
        Err(_) => println!("  (dir not present \u{2014} hooks not installed or never fired)"),
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_paths_with_spaces() {
        // The bug: an unquoted path with a space was word-split by `sh -c`, so
        // `/mnt/c/Users/Ben Bracamonte/…/herdr` ran as just `/mnt/c/Users/Ben`.
        let q = shell_single_quote("/mnt/c/Users/Ben Bracamonte/Work/herdr/target/release/herdr");
        assert_eq!(
            q,
            "'/mnt/c/Users/Ben Bracamonte/Work/herdr/target/release/herdr'"
        );
    }

    #[test]
    fn escapes_embedded_single_quotes() {
        assert_eq!(shell_single_quote("a'b"), "'a'\\''b'");
    }

    #[test]
    fn command_keeps_bare_hook_notify_suffix() {
        // `hookstate::is_ours` recognizes our entry by this suffix, so re-install can
        // replace an older unquoted one. The suffix must stay outside the quotes.
        assert!(hook_command().ends_with(" hook notify"));
    }
}
