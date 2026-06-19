use crate::session::{ClaudeSession, SessionStatus};

/// Check which PIDs are alive and fetch TTY, CPU%, MEM, command args — all via `ps`.
/// No sysinfo dependency needed.
pub fn fetch_and_enrich(sessions: &mut [ClaudeSession]) {
    if sessions.is_empty() {
        return;
    }

    let pids: Vec<String> = sessions.iter().map(|s| s.pid.to_string()).collect();
    let pid_arg = pids.join(",");

    let output = std::process::Command::new("ps")
        .args(["-o", "pid=,tty=,%cpu=,rss=,command=", "-p", &pid_arg])
        .env_clear()
        .output();

    let output = match output {
        Ok(o) => o,
        Err(e) => {
            crate::logger::log("ERROR", &format!("ps command failed: {e}"));
            // ps failed — mark all as Finished (will show tombstone for 30s)
            for s in sessions.iter_mut() {
                s.status = SessionStatus::Finished;
                s.cpu_percent = 0.0;
            }
            return;
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Collect alive PIDs from ps output
    let mut alive_pids = std::collections::HashSet::new();

    for line in stdout.lines() {
        let trimmed = line.trim();
        let fields: Vec<&str> = trimmed.split_whitespace().collect();
        if fields.len() < 5 {
            continue;
        }
        let Ok(pid) = fields[0].parse::<u32>() else {
            continue;
        };
        let tty = fields[1].to_string();
        let cpu = fields[2].parse::<f32>().unwrap_or(0.0);
        let rss_kb = fields[3].parse::<f64>().unwrap_or(0.0);
        let mem_mb = rss_kb / 1024.0;
        let command = fields[4..].join(" ");

        // Only count this PID as alive if it's actually a claude process.
        // PIDs get reused on macOS — a dead claude session's PID may belong
        // to an unrelated process now.
        if !command.contains("claude") {
            continue;
        }

        alive_pids.insert(pid);

        for session in sessions.iter_mut() {
            if session.pid == pid {
                session.tty = tty.clone();
                session.mem_mb = mem_mb;

                // CPU smoothing: track last 3 readings, use average
                session.cpu_history.push(cpu);
                if session.cpu_history.len() > 3 {
                    session.cpu_history.remove(0);
                }
                session.cpu_percent =
                    session.cpu_history.iter().sum::<f32>() / session.cpu_history.len() as f32;

                // Extract args (everything after "claude")
                if let Some(idx) = command.find("claude") {
                    let after_claude = &command[idx + 6..];
                    session.command_args = after_claude.trim().to_string();
                }

                // Extract session name from --name or --resume
                let cmd_parts: Vec<&str> = command.split_whitespace().collect();
                extract_session_meta(&cmd_parts, session);

                break;
            }
        }
    }

    // Mark dead PIDs as Finished instead of removing them immediately.
    // They'll be displayed briefly so the user can see what exited.
    for session in sessions.iter_mut() {
        if !alive_pids.contains(&session.pid) {
            session.status = crate::session::SessionStatus::Finished;
            session.cpu_percent = 0.0;
        }
    }
}

fn extract_session_meta(cmd: &[&str], session: &mut ClaudeSession) {
    let mut i = 0;
    while i < cmd.len() {
        match cmd[i] {
            "--name" | "-n" if i + 1 < cmd.len() => {
                session.session_name = cmd[i + 1].to_string();
                i += 2;
                continue;
            }
            "--resume" | "-r" if i + 1 < cmd.len() => {
                let val = cmd[i + 1];
                if !looks_like_uuid(val) {
                    session.session_name = val.to_string();
                }
                i += 2;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
}

fn looks_like_uuid(s: &str) -> bool {
    s.len() == 36
        && s.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
        && s.matches('-').count() == 4
}

/// Terminate a process by PID with SIGTERM (graceful). Returns Ok if the signal
/// was delivered, Err with the OS error otherwise (e.g. no such process, or no
/// permission). herdr drives this for the `d` (kill) action — the dormant
/// `MockRuntime::terminate_session` is a no-op, so the real kill lives here.
pub fn terminate(pid: u32) -> Result<(), String> {
    signal(pid, libc::SIGTERM)
}

/// Force-kill a process with SIGKILL (uncatchable) — the escalation when a
/// graceful SIGTERM was ignored.
pub fn force_kill(pid: u32) -> Result<(), String> {
    signal(pid, libc::SIGKILL)
}

fn signal(pid: u32, sig: libc::c_int) -> Result<(), String> {
    let ret = unsafe { libc::kill(pid as libc::pid_t, sig) };
    if ret == 0 {
        Ok(())
    } else {
        Err(format!("kill {pid}: {}", std::io::Error::last_os_error()))
    }
}

#[cfg(test)]
mod terminate_tests {
    use super::{force_kill, terminate};
    use std::process::Command;
    use std::time::{Duration, Instant};

    fn wait_exit(child: &mut std::process::Child, what: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if child.try_wait().expect("try_wait").is_some() {
                return;
            }
            assert!(Instant::now() < deadline, "{what}");
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    #[test]
    fn force_kill_ends_a_child() {
        // SIGKILL is the escalation for an agent that ignored SIGTERM. Prove the
        // signal is delivered and ends the process.
        let mut child = Command::new("sleep").arg("30").spawn().expect("spawn sleep");
        let pid = child.id();
        force_kill(pid).expect("SIGKILL delivered");
        wait_exit(&mut child, "SIGKILL should end the child");
    }

    #[test]
    fn terminate_kills_a_real_child() {
        // Spawn a long sleep, SIGTERM it, and confirm it exits.
        let mut child = Command::new("sleep").arg("60").spawn().expect("spawn sleep");
        let pid = child.id();
        terminate(pid).expect("SIGTERM should be delivered");

        // Poll try_wait until the child is reaped (SIGTERM ends `sleep`).
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if child.try_wait().expect("try_wait").is_some() {
                break;
            }
            assert!(Instant::now() < deadline, "child should have exited after SIGTERM");
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    #[test]
    fn terminate_missing_pid_errors() {
        // PID 0 is the caller's process group — SIGTERM to a clearly-invalid high
        // PID should fail with ESRCH rather than signal anything.
        assert!(terminate(2_000_000_000).is_err());
    }
}
