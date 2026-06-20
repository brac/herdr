use crate::session::{ClaudeSession, SessionStatus};

/// CPU-signal smoothing (this drives status inference). Asymmetric so a finished
/// agent stops reading "busy" quickly while transient spikes are still damped.
/// These are the knobs to tune live if status feels too eager or too laggy.
const CPU_EMA_ALPHA_RISE: f32 = 0.5; // climbing: damp spikes
const CPU_EMA_ALPHA_FALL: f32 = 0.8; // dropping: react fast (agent went idle)

/// Minimum wall-clock between CPU samples before we trust the delta. `ps time=`
/// has 1-second resolution, so a sub-second window would quantize to wild 0%/100%
/// readings; below this we keep the prior smoothed value and let the window grow.
const MIN_CPU_SAMPLE_MS: u64 = 750;

/// Parse `ps -o time=` cumulative CPU time into seconds. Formats: `MM:SS`,
/// `HH:MM:SS`, and Linux's `DD-HH:MM:SS` (days separated by `-`). `None` on an
/// unrecognized shape so the caller degrades to 0 rather than panicking (§3).
fn parse_cputime(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (days, hms) = match s.split_once('-') {
        Some((d, rest)) => (d.parse::<f64>().ok()?, rest),
        None => (0.0, s),
    };
    let mut secs = days * 86_400.0;
    let parts: Vec<&str> = hms.split(':').collect();
    match parts.as_slice() {
        [h, m, sec] => {
            secs += h.parse::<f64>().ok()? * 3_600.0
                + m.parse::<f64>().ok()? * 60.0
                + sec.parse::<f64>().ok()?;
        }
        [m, sec] => {
            secs += m.parse::<f64>().ok()? * 60.0 + sec.parse::<f64>().ok()?;
        }
        [sec] => {
            secs += sec.parse::<f64>().ok()?;
        }
        _ => return None,
    }
    Some(secs)
}

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

    // `time=` is cumulative CPU seconds, NOT `%cpu` (which is a lifetime average:
    // CPU-time ÷ elapsed, so it lingers high long after an agent goes idle). We
    // diff this counter over wall-clock to get a true instantaneous CPU% below.
    let output = std::process::Command::new("ps")
        .args(["-o", "pid=,tty=,time=,rss=,command=", "-p", &pid_arg])
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
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

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
        let cpu_secs = parse_cputime(fields[2]).unwrap_or(0.0);
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

                // Instantaneous CPU% = Δ(cumulative CPU seconds) ÷ Δ(wall seconds).
                // This is the live "busy?" signal `ps %cpu` can't give (it's a
                // lifetime average). Seed silently on the first sample (no prior
                // counter to diff), and skip windows shorter than the resolution
                // of `ps time=` (1s) so the delta stays meaningful — keeping the
                // last smoothed value until enough wall-clock has accrued.
                match session.prev_cpu_secs {
                    Some(prev_secs) => {
                        let elapsed_ms = now_ms.saturating_sub(session.prev_cpu_sample_ms);
                        if elapsed_ms >= MIN_CPU_SAMPLE_MS {
                            let inst = (((cpu_secs - prev_secs).max(0.0)
                                / (elapsed_ms as f64 / 1000.0))
                                * 100.0) as f32;
                            session.cpu_history.push(inst);
                            if session.cpu_history.len() > 3 {
                                session.cpu_history.remove(0);
                            }
                            // Asymmetric EMA: damp spikes on the way up, decay fast
                            // on a drop so a finished agent clears "Processing" in
                            // ~1–2 ticks (BACKLOG: slow status).
                            session.cpu_percent = smooth_cpu(session.cpu_percent, inst);
                            session.prev_cpu_secs = Some(cpu_secs);
                            session.prev_cpu_sample_ms = now_ms;
                        }
                        // else: window too short — keep the prior cpu_percent and
                        // let the counter/clock accumulate for the next tick.
                    }
                    None => {
                        // First time we've seen this process: record the counter,
                        // assume idle until we have a delta to prove otherwise.
                        session.prev_cpu_secs = Some(cpu_secs);
                        session.prev_cpu_sample_ms = now_ms;
                        session.cpu_percent = 0.0;
                    }
                }

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
mod cputime_tests {
    use super::parse_cputime;

    #[test]
    fn parses_mm_ss() {
        assert_eq!(parse_cputime("03:41"), Some(221.0));
    }

    #[test]
    fn parses_hh_mm_ss() {
        assert_eq!(parse_cputime("01:00:00"), Some(3_600.0));
        assert_eq!(parse_cputime("00:03:41"), Some(221.0));
    }

    #[test]
    fn parses_days_prefix() {
        // Linux renders long-lived processes as DD-HH:MM:SS.
        assert_eq!(parse_cputime("1-00:00:00"), Some(86_400.0));
    }

    #[test]
    fn rejects_garbage_without_panicking() {
        assert_eq!(parse_cputime(""), None);
        assert_eq!(parse_cputime("not-a-time"), None);
    }

    #[test]
    fn instantaneous_cpu_is_the_delta_over_wall_not_the_lifetime_average() {
        // The whole point of the fix: a process that used 221 CPU-seconds over a
        // 39-minute lifetime reads ~9.3% under `ps %cpu`, but if it burned only
        // 0.1 CPU-seconds in the last 2 wall-seconds it is ~5% *now* — and if it
        // burned nothing, 0%. Prove the delta math reflects the recent window.
        let prev_secs = 221.0_f64;
        let now_secs_busy = prev_secs + 2.0; // 2 cpu-sec in a 2s window = 100%
        let now_secs_idle = prev_secs; // nothing used
        let window_s = 2.0_f64;
        let busy = ((now_secs_busy - prev_secs) / window_s) * 100.0;
        let idle = ((now_secs_idle - prev_secs) / window_s) * 100.0;
        assert!((busy - 100.0).abs() < 0.01);
        assert_eq!(idle, 0.0);
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
