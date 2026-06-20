use crate::session::{ClaudeSession, SessionStatus};

/// CPU-signal smoothing (this drives status inference). Asymmetric so a finished
/// agent stops reading "busy" quickly while transient spikes are still damped.
/// These are the knobs to tune live if status feels too eager or too laggy.
const CPU_EMA_ALPHA_RISE: f32 = 0.5; // climbing: damp spikes
const CPU_EMA_ALPHA_FALL: f32 = 0.8; // dropping: react fast (agent went idle)

/// Asymmetric exponential moving average for a session's CPU%. `prev` is the
/// previous smoothed value, `sample` the fresh `ps` reading. Rising samples are
/// damped (a lone spike shouldn't flip status to Processing); falling samples
/// decay quickly so "Processing" clears within ~1–2 ticks of the work finishing,
/// instead of the ~6s the old 3-sample mean took (BACKLOG: slow status/activity).
fn smooth_cpu(prev: f32, sample: f32) -> f32 {
    let alpha = if sample >= prev {
        CPU_EMA_ALPHA_RISE
    } else {
        CPU_EMA_ALPHA_FALL
    };
    alpha * sample + (1.0 - alpha) * prev
}

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

                // CPU smoothing: asymmetric EMA — damp spikes on the way up, but
                // decay fast on a drop so a finished agent stops reading "busy"
                // within ~1–2 ticks instead of lingering on Processing for ~6s
                // (BACKLOG: slow status). The short raw history detects the first
                // sample (seed without smoothing) and remains for diagnostics.
                let first_sample = session.cpu_history.is_empty();
                session.cpu_history.push(cpu);
                if session.cpu_history.len() > 3 {
                    session.cpu_history.remove(0);
                }
                session.cpu_percent = if first_sample {
                    cpu
                } else {
                    smooth_cpu(session.cpu_percent, cpu)
                };

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
mod smoothing_tests {
    use super::smooth_cpu;

    #[test]
    fn damps_a_rising_spike() {
        // On the way up a lone high reading isn't taken at face value.
        let v = smooth_cpu(0.0, 80.0);
        assert!(v > 0.0 && v < 80.0, "rise is damped, got {v}");
        assert!((v - 40.0).abs() < 0.01, "rise uses the rise alpha (0.5), got {v}");
    }

    #[test]
    fn stays_responsive_enough_to_show_processing() {
        // Even damped, a busy agent must clear the Processing threshold (>5%) at once.
        assert!(smooth_cpu(0.0, 80.0) > 5.0);
    }

    #[test]
    fn decays_fast_when_the_agent_goes_idle() {
        // BACKLOG slow-status: busy → idle must clear the Processing threshold (5%)
        // within ~2 ticks instead of lingering ~6s like the old 3-sample mean.
        let t1 = smooth_cpu(80.0, 0.0);
        let t2 = smooth_cpu(t1, 0.0);
        assert!(t1 < 80.0, "decaying, got {t1}");
        assert!(t2 < 5.0, "clears Processing within two idle ticks, got {t2}");
    }

    #[test]
    fn a_single_mid_task_dip_does_not_flip_status() {
        // One idle sample between bursts keeps a busy agent above the threshold,
        // so status doesn't flicker out of Processing mid-task.
        let after_dip = smooth_cpu(40.0, 0.0);
        assert!(after_dip > 5.0, "one dip keeps it busy, got {after_dip}");
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
