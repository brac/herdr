use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};

use serde_json::Value;

use crate::models;
use crate::session::{
    ChatKind, ChatRole, ClaudeSession, SeenUsage, SessionStatus, SubagentRollup, TelemetryStatus,
};
use crate::transcript::{
    TranscriptBlock, TranscriptEvent, TranscriptRole, TranscriptUsage, parse_line,
};
use std::collections::HashMap;

/// Build the dedup key from a message's id + requestId: `message.id` with `requestId` as
/// a tiebreaker. Returns `None` when there's no id — such a line can't be deduped and is
/// counted in full. Takes the fields by ref (not the whole message) so it composes with
/// the parent loop's partial moves of `stop_reason`/`usage`.
fn dedup_key(id: Option<&str>, request_id: Option<&str>) -> Option<String> {
    id.map(|id| match request_id {
        Some(req) => format!("{id}:{req}"),
        None => id.to_string(),
    })
}

/// Apply streaming-duplicate dedup for one usage line and return the per-tier amounts
/// to ADD to the running totals: full tokens for a first-seen message, or only the
/// increase over the previous max for a re-emitted one (keyless lines add in full).
fn merge_usage(
    seen: &mut HashMap<String, SeenUsage>,
    key: Option<&str>,
    usage: &TranscriptUsage,
) -> SeenUsage {
    let Some(key) = key else {
        return SeenUsage {
            input: usage.input_tokens,
            output: usage.output_tokens,
            cache_read: usage.cache_read_input_tokens,
            cache_write: usage.cache_creation_input_tokens,
        };
    };
    let entry = seen.entry(key.to_string()).or_default();
    let delta = SeenUsage {
        input: usage.input_tokens.saturating_sub(entry.input),
        output: usage.output_tokens.saturating_sub(entry.output),
        cache_read: usage.cache_read_input_tokens.saturating_sub(entry.cache_read),
        cache_write: usage
            .cache_creation_input_tokens
            .saturating_sub(entry.cache_write),
    };
    entry.input = entry.input.max(usage.input_tokens);
    entry.output = entry.output.max(usage.output_tokens);
    entry.cache_read = entry.cache_read.max(usage.cache_read_input_tokens);
    entry.cache_write = entry.cache_write.max(usage.cache_creation_input_tokens);
    delta
}

#[derive(Default)]
struct UsageRollup {
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    cost_usd: f64,
    usage_metrics_available: bool,
    cost_estimate_unverified: bool,
}

impl UsageRollup {
    fn total_input_tokens(&self) -> u64 {
        self.input_tokens + self.cache_read_tokens + self.cache_write_tokens
    }
}

/// Read new JSONL entries since last offset, accumulate token stats.
pub fn update_tokens(session: &mut ClaudeSession) {
    // Seed from persisted state so status inference works on ticks with no new JSONL.
    let mut last_type = session.last_msg_type.clone();
    let mut last_stop_reason = session.last_stop_reason.clone();
    let mut is_waiting_for_task = session.is_waiting_for_task;
    let mut last_was_api_error = session.last_was_api_error;
    let mut saw_non_empty_line = false;
    let mut recognized_events = 0usize;
    let mut saw_parent_usage = false;
    let jsonl_path = session.jsonl_path.clone();

    match jsonl_path.as_ref() {
        Some(path) => {
            let mut file = match File::open(path) {
                Ok(f) => f,
                Err(_) => {
                    session.telemetry_status = TelemetryStatus::UnreadableTranscript;
                    finalize_usage(
                        session,
                        &last_type,
                        &last_stop_reason,
                        is_waiting_for_task,
                        last_was_api_error,
                        false,
                    );
                    return;
                }
            };

            let file_len = file.metadata().map(|m| m.len()).unwrap_or(0);

            if file_len == 0 {
                session.telemetry_status = TelemetryStatus::Pending;
            } else {
                if session.jsonl_offset > file_len {
                    session.jsonl_offset = 0;
                    session.own_input_tokens = 0;
                    session.own_output_tokens = 0;
                    session.own_cache_read_tokens = 0;
                    session.own_cache_write_tokens = 0;
                    session.usage_seen.clear();
                    // Reset persisted inference state on file truncation
                    last_type.clear();
                    last_stop_reason.clear();
                    is_waiting_for_task = false;
                    last_was_api_error = false;
                }

                if session.jsonl_offset < file_len {
                    if session.jsonl_offset > 0
                        && file.seek(SeekFrom::Start(session.jsonl_offset)).is_err()
                    {
                        finalize_usage(
                            session,
                            &last_type,
                            &last_stop_reason,
                            is_waiting_for_task,
                            last_was_api_error,
                            false,
                        );
                        return;
                    }

                    let reader = BufReader::new(&file);

                    for line in reader.lines() {
                        let line = match line {
                            Ok(l) => l,
                            Err(_) => break,
                        };

                        if line.trim().is_empty() {
                            continue;
                        }
                        saw_non_empty_line = true;

                        let Some(event) = parse_line(&line) else {
                            continue;
                        };
                        recognized_events += 1;

                        match event {
                            TranscriptEvent::WaitingForTask => {
                                is_waiting_for_task = true;
                            }
                            TranscriptEvent::ApiError { text } => {
                                // Sticky until a newer message supersedes it.
                                last_was_api_error = true;
                                session.last_api_error_msg = text;
                            }
                            TranscriptEvent::Message(message) => {
                                is_waiting_for_task = false;
                                // A real message after an error means it cleared
                                // (retry succeeded or a new turn began).
                                last_was_api_error = false;
                                last_type = match message.role {
                                    TranscriptRole::Assistant => "assistant".to_string(),
                                    TranscriptRole::User => "user".to_string(),
                                };

                                if let Some(reason) = message.stop_reason {
                                    last_stop_reason = reason;
                                } else {
                                    // Claude Code sometimes writes assistant messages
                                    // with stop_reason: null when a tool_use block is
                                    // awaiting user approval.  Infer from content.
                                    let has_tool_use = message
                                        .content
                                        .iter()
                                        .any(|b| matches!(b, TranscriptBlock::ToolUse { .. }));
                                    if has_tool_use {
                                        last_stop_reason = "tool_use".to_string();
                                    } else {
                                        last_stop_reason.clear();
                                    }
                                }

                                let dedup =
                                    dedup_key(message.id.as_deref(), message.request_id.as_deref());
                                if let Some(usage) = message.usage {
                                    // Streaming-duplicate dedup: add only the increase over
                                    // any prior emission of this (message.id, requestId).
                                    let add =
                                        merge_usage(&mut session.usage_seen, dedup.as_deref(), &usage);
                                    session.own_input_tokens +=
                                        add.input + add.cache_read + add.cache_write;
                                    session.own_output_tokens += add.output;
                                    session.own_cache_read_tokens += add.cache_read;
                                    session.own_cache_write_tokens += add.cache_write;
                                    saw_parent_usage = true;

                                    // Context window = the LAST API call's actual prompt size
                                    // (the raw line value, not a dedup delta).
                                    let context_size = usage.input_tokens
                                        + usage.cache_read_input_tokens
                                        + usage.cache_creation_input_tokens;
                                    if context_size > 0 {
                                        session.context_tokens = context_size;
                                    }
                                }

                                if let Some(model) = message.model {
                                    session.model = shorten_model(&model);
                                }

                                // Phase 4: retain content for the conversation view.
                                let chat_role = match message.role {
                                    TranscriptRole::Assistant => ChatRole::Assistant,
                                    TranscriptRole::User => ChatRole::User,
                                };

                                for block in message.content {
                                    match &block {
                                        TranscriptBlock::Text(text) => {
                                            session.push_chat(
                                                chat_role,
                                                ChatKind::Text,
                                                text.clone(),
                                            );
                                        }
                                        TranscriptBlock::ToolUse { name, input } => {
                                            session.push_chat(
                                                ChatRole::Assistant,
                                                ChatKind::Tool,
                                                summarize_tool(name, input),
                                            );
                                        }
                                        // Tool results are noisy; the chat view skips them.
                                        TranscriptBlock::ToolResult { .. } => {}
                                    }
                                    match &block {
                                        TranscriptBlock::ToolUse { name, input } => {
                                            record_tool_usage(name, input, session);
                                            // Track pending tool for rule-based auto-actions
                                            session.pending_tool_name = Some(name.clone());
                                            session.pending_tool_input = input
                                                .get("command")
                                                .and_then(|v| v.as_str())
                                                .map(|s| s.to_string());
                                            // Track pending file path for conflict detection
                                            session.pending_file_path = if matches!(
                                                name.as_str(),
                                                "Edit" | "Write" | "NotebookEdit"
                                            ) {
                                                input
                                                    .get("file_path")
                                                    .and_then(|v| v.as_str())
                                                    .map(|s| s.to_string())
                                            } else {
                                                None
                                            };
                                        }
                                        TranscriptBlock::ToolResult {
                                            is_error, content, ..
                                        } => {
                                            session.last_tool_error = *is_error;
                                            if *is_error {
                                                session.total_error_count += 1;
                                                session.current_window_errors += 1;
                                                let truncated = if content.len() > 256 {
                                                    format!(
                                                        "{}...",
                                                        crate::session::truncate_str(content, 256)
                                                    )
                                                } else {
                                                    content.clone()
                                                };
                                                let tool_name = session
                                                    .pending_tool_name
                                                    .clone()
                                                    .unwrap_or_else(|| "?".into());
                                                session.last_error_message =
                                                    Some(truncated.clone());
                                                session.recent_errors.push(
                                                    crate::session::ErrorEntry {
                                                        tool_name,
                                                        message: truncated,
                                                    },
                                                );
                                                if session.recent_errors.len() > 5 {
                                                    session.recent_errors.remove(0);
                                                }
                                            } else {
                                                session.last_error_message = None;
                                            }
                                            // Tool was executed — no longer pending
                                            session.pending_tool_name = None;
                                            session.pending_tool_input = None;
                                            session.pending_file_path = None;
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
                }

                if recognized_events > 0 || session.telemetry_status.is_available() {
                    session.telemetry_status = TelemetryStatus::Available;
                } else if saw_non_empty_line {
                    session.telemetry_status = TelemetryStatus::UnsupportedTranscript;
                } else {
                    session.telemetry_status = TelemetryStatus::Pending;
                }

                session.jsonl_offset = file_len;
            }

            if let Ok(meta) = std::fs::metadata(path) {
                if let Ok(modified) = meta.modified() {
                    let mtime_ms = modified
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64;
                    session.last_message_ts = mtime_ms;
                }
            }
        }
        None => {
            session.telemetry_status = TelemetryStatus::MissingTranscript;
        }
    }

    finalize_usage(
        session,
        &last_type,
        &last_stop_reason,
        is_waiting_for_task,
        last_was_api_error,
        saw_parent_usage,
    );
}

fn finalize_usage(
    session: &mut ClaudeSession,
    last_type: &str,
    last_stop_reason: &str,
    is_waiting_for_task: bool,
    last_was_api_error: bool,
    saw_parent_usage: bool,
) {
    let resolved_profile = models::resolve(&session.model);
    session.context_max = resolved_profile.profile.context_max;
    session.model_profile_source = resolved_profile.source.label().to_string();

    let subagent_rollup = refresh_subagent_rollups(session);
    session.subagent_input_tokens = subagent_rollup.total_input_tokens();
    session.subagent_output_tokens = subagent_rollup.output_tokens;
    session.subagent_cache_read_tokens = subagent_rollup.cache_read_tokens;
    session.subagent_cache_write_tokens = subagent_rollup.cache_write_tokens;
    session.subagent_count = session.subagent_rollups.len();

    session.total_input_tokens = session.own_input_tokens + session.subagent_input_tokens;
    session.total_output_tokens = session.own_output_tokens + session.subagent_output_tokens;
    session.cache_read_tokens = session.own_cache_read_tokens + session.subagent_cache_read_tokens;
    session.cache_write_tokens =
        session.own_cache_write_tokens + session.subagent_cache_write_tokens;

    let own_usage_metrics_available = saw_parent_usage
        || session.own_input_tokens > 0
        || session.own_output_tokens > 0
        || session.own_cache_read_tokens > 0
        || session.own_cache_write_tokens > 0;
    let (own_cost, own_cost_unverified) = estimate_cost_components(
        &session.model,
        session.own_input_tokens,
        session.own_output_tokens,
        session.own_cache_read_tokens,
        session.own_cache_write_tokens,
    );
    session.cost_usd = own_cost + subagent_rollup.cost_usd;
    session.usage_metrics_available =
        own_usage_metrics_available || subagent_rollup.usage_metrics_available;
    session.cost_estimate_unverified = (own_usage_metrics_available && own_cost_unverified)
        || subagent_rollup.cost_estimate_unverified;

    // Persist for next tick (so status inference works when no new JSONL arrives).
    session.last_msg_type = last_type.to_string();
    session.last_stop_reason = last_stop_reason.to_string();
    session.is_waiting_for_task = is_waiting_for_task;
    session.last_was_api_error = last_was_api_error;

    infer_status(
        session,
        last_type,
        last_stop_reason,
        is_waiting_for_task,
        last_was_api_error,
    );

    // Phase B: if an opt-in Notification/Stop hook left a fresh status file, let it
    // override the heuristic — turning the invisible-permission-prompt guess into a
    // fact. No-op when hooks aren't installed (the file simply doesn't exist).
    apply_hook_override(session);

    // Status diagnostics (no-op unless HERDR_LOG is set; see logger::init). The
    // signals here are exactly what infer_status branched on, so a stuck status
    // is readable straight from the log.
    crate::logger::log(
        "DEBUG",
        &format!(
            "status pid={} -> {:?} | cpu={:.1}% msg_type={} stop={} waiting_for_task={}",
            session.pid,
            session.status,
            session.cpu_percent,
            if last_type.is_empty() { "-" } else { last_type },
            if last_stop_reason.is_empty() {
                "-"
            } else {
                last_stop_reason
            },
            is_waiting_for_task,
        ),
    );
}

/// Freshness window for an inbound hook status file. Beyond this, treat it as a
/// leftover from a previous run and ignore it (the agent would have written newer
/// JSONL by now anyway).
const HOOK_FRESH_MS: u64 = 5 * 60 * 1000;

/// Apply an opt-in Notification/Stop hook's status file as an authoritative override
/// of the heuristic — but only while it's the most recent thing that happened. Once
/// the agent resumes and writes new JSONL, `last_message_ts` (the transcript mtime)
/// advances past the hook and the heuristic takes back over, so this self-clears
/// without anyone deleting the file.
fn apply_hook_override(session: &mut ClaudeSession) {
    use crate::hookstate::{self, HookStatus};

    let Some(state) = hookstate::read(&session.session_id) else {
        return;
    };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    // Stale leftover, or superseded by newer transcript activity (500ms slop: Stop
    // fires right after the final line is written) → defer to the heuristic.
    if now_ms.saturating_sub(state.ts_ms) > HOOK_FRESH_MS {
        return;
    }
    if state.ts_ms + 500 < session.last_message_ts {
        return;
    }

    match state.status {
        // A fresh Notification means the agent is blocked on the user — make it a
        // fact. It outranks the heuristic's inferred status, INCLUDING the low-CPU
        // "Processing" the tool_use branch now reports during a just-called tool's
        // grace window, so a real permission prompt still surfaces instantly when
        // hooks are on. It defers only to a visibly-busy agent (high CPU = really
        // working, so the hook must be stale/superseded) or an active Error.
        HookStatus::NeedsInput => {
            let busy = session.cpu_percent > 5.0;
            if !busy && session.status != SessionStatus::Error {
                session.status = SessionStatus::NeedsInput;
            }
        }
        // Confirm a finished turn only when the heuristic was ambiguous (idle/waiting/
        // unknown) — never stomp Processing/NeedsInput/Error.
        HookStatus::JobDone => {
            if matches!(
                session.status,
                SessionStatus::Idle | SessionStatus::WaitingInput | SessionStatus::Unknown
            ) {
                session.status = SessionStatus::JobDone;
            }
        }
    }
}

/// How long (minutes) a finished session sits before the roster calls it Idle
/// rather than just Waiting. Shared by the end_turn and user/tool_result paths.
const IDLE_AFTER_MINS: u64 = 10;
/// Grace window (seconds) after a `user`/tool_result line during which low CPU
/// still reads as Processing — covers the brief plan-the-next-tool gap between a
/// tool result landing and the model's next streamed message. Past this, low CPU
/// at a user line means the turn is over (Claude Code agents routinely end a turn
/// on a tool call, parking the transcript at a tool_result) → Waiting, not a
/// permanent false Processing.
const USER_LINE_PROCESSING_GRACE_SECS: u64 = 10;
/// How long (seconds) a tool call must sit outstanding with the agent idle (CPU
/// low) before the heuristic calls it NeedsInput. An auto-approved tool that is
/// merely *executing* (a long Bash/web call) looks identical to a permission
/// prompt from CPU + JSONL alone — the Claude process is idle while the tool
/// runs — so flipping to NeedsInput the instant a tool is called flickers: it
/// flashes NeedsInput, then snaps back to Processing when the ToolResult lands.
/// Waiting out a normal tool round-trip suppresses that false positive (BACKLOG
/// "Needs Input then it changes to processing"). The inbound Notification hook,
/// when installed, makes a real permission prompt instant and authoritative
/// (see `apply_hook_override`), so this gate only governs the no-hook heuristic.
const NEEDS_INPUT_STALE_SECS: u64 = 10;

pub fn infer_status(
    session: &mut ClaudeSession,
    last_msg_type: &str,
    last_stop_reason: &str,
    is_waiting_for_task: bool,
    last_was_api_error: bool,
) {
    // CPU is the strongest real-time signal — if the process is burning CPU,
    // it's processing regardless of what the JSONL says (JSONL can lag). This
    // also covers an automatic retry after an API error: while it's working,
    // show Processing rather than a stale Error.
    if session.cpu_percent > 5.0 {
        session.status = SessionStatus::Processing;
        return;
    }

    // The last transcript event was an API error and nothing newer has
    // superseded it (and the process isn't actively retrying) → surface Error
    // so it's visible in the roster (BACKLOG "Error Display").
    if last_was_api_error {
        session.status = SessionStatus::Error;
        return;
    }

    // NeedsInput: JSONL says waiting_for_task and CPU is low (confirmed idle)
    if is_waiting_for_task {
        session.status = SessionStatus::NeedsInput;
        return;
    }

    if !session.telemetry_status.is_available() && last_msg_type.is_empty() {
        session.status = SessionStatus::Unknown;
        return;
    }

    if last_msg_type == "assistant" && last_stop_reason == "end_turn" {
        // Claude finished its turn — the task is done and it's waiting for the
        // user's next instruction. That's distinct from "Waiting" (a request in
        // flight to the API): here nothing is happening until the user acts, so
        // report JobDone (BACKLOG "Split waiting states"). After a long gap, Idle.
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let age_mins = (now_ms.saturating_sub(session.last_message_ts)) / 60_000;

        if age_mins > IDLE_AFTER_MINS {
            session.status = SessionStatus::Idle;
        } else {
            session.status = SessionStatus::JobDone;
        }
        return;
    }

    if last_msg_type == "assistant" && last_stop_reason == "tool_use" {
        // Claude called a tool. If CPU is non-trivial it's actively working the
        // tool/result → Processing (the cpu>5 fast path above already claimed
        // streaming; this tighter 2.0 cutoff is the "idle enough to be blocked"
        // line). Otherwise the agent is idle with a tool call outstanding.
        if session.cpu_percent >= 2.0 {
            session.status = SessionStatus::Processing;
            return;
        }
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let age_secs = (now_ms.saturating_sub(session.last_message_ts)) / 1000;
        if session.pending_tool_name.is_some() {
            // A ToolUse with no ToolResult yet, agent idle: EITHER a permission
            // prompt (truly blocked on the user) OR an auto-approved tool still
            // executing. Indistinguishable from CPU + JSONL alone, so don't cry
            // NeedsInput until the call has outlived a normal round-trip — this
            // is the fix for the "NeedsInput flashes then flips to Processing"
            // bug. Hooks, when installed, surface a real prompt instantly anyway.
            if age_secs > NEEDS_INPUT_STALE_SECS {
                session.status = SessionStatus::NeedsInput;
            } else {
                session.status = SessionStatus::Processing;
            }
        } else if age_secs / 60 > IDLE_AFTER_MINS {
            // No tool outstanding (result already in) and long idle → Idle.
            session.status = SessionStatus::Idle;
        } else {
            // Result in, model composing its next message → Processing.
            session.status = SessionStatus::Processing;
        }
        return;
    }

    if last_msg_type == "user" {
        // The last transcript line is a user/tool_result message, so Claude owes
        // a response. Distinguish "actively generating" from "turn's over": the
        // top-of-function `cpu > 5` check already claimed active streaming, so by
        // here CPU is low. Two sub-cases remain:
        //   - fresh (within the grace window): the model is between a tool result
        //     and its next streamed message → Processing.
        //   - aged out: a Claude Code agent routinely ENDS a turn on a tool call,
        //     leaving the transcript parked at a tool_result while it waits for
        //     the next human prompt. Without an age check this branch reported
        //     Processing forever (the "stuck Processing at $0/hr" bug). Treat it
        //     like a finished turn: Waiting, then Idle after a long gap.
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let age_ms = now_ms.saturating_sub(session.last_message_ts);
        let age_secs = age_ms / 1000;

        if session.cpu_percent > 1.0 || age_secs < USER_LINE_PROCESSING_GRACE_SECS {
            session.status = SessionStatus::Processing;
        } else if age_ms / 60_000 > IDLE_AFTER_MINS {
            session.status = SessionStatus::Idle;
        } else {
            session.status = SessionStatus::WaitingInput;
        }
        return;
    }

    session.status = SessionStatus::Idle;
}

/// Estimate USD cost based on token usage and model.
#[allow(dead_code)]
pub fn estimate_cost(session: &ClaudeSession) -> f64 {
    estimate_cost_components(
        &session.model,
        session.total_input_tokens,
        session.total_output_tokens,
        session.cache_read_tokens,
        session.cache_write_tokens,
    )
    .0
}

/// Max context window tokens by model.
pub fn model_context_max(model: &str) -> u64 {
    models::resolve(model).profile.context_max
}

/// Extract tool usage stats and file paths from tool_use content blocks.
/// A compact one-line summary of a tool call for the chat view, e.g.
/// `Edit: src/lib.rs` or `Bash: cargo test`. Falls back to the tool name.
fn summarize_tool(name: &str, input: &Value) -> String {
    let detail = input
        .get("file_path")
        .or_else(|| input.get("path"))
        .or_else(|| input.get("command"))
        .or_else(|| input.get("pattern"))
        .or_else(|| input.get("query"))
        .or_else(|| input.get("url"))
        .and_then(|v| v.as_str());
    match detail {
        Some(d) if !d.is_empty() => {
            let d = d.replace('\n', " ");
            format!("{name}: {}", crate::session::truncate_str(&d, 60))
        }
        _ => name.to_string(),
    }
}

fn record_tool_usage(tool_name: &str, input: &Value, session: &mut ClaudeSession) {
    if tool_name.is_empty() {
        return;
    }

    session
        .tool_usage
        .entry(tool_name.to_string())
        .or_default()
        .calls += 1;

    if matches!(tool_name, "Edit" | "Write" | "NotebookEdit") {
        if let Some(path) = input.get("file_path").and_then(|p| p.as_str()) {
            *session.files_modified.entry(path.to_string()).or_insert(0) += 1;
            // Reset file-read tracker for this path (it was just edited)
            session.file_reads_since_edit.remove(path);
        }
        // Track token efficiency: cumulative tokens at each edit event
        let total_tokens = session.total_input_tokens + session.total_output_tokens;
        session.total_tokens_at_edit_count += total_tokens;
        session.edit_event_count += 1;
        // Freeze baseline tokens-per-edit after first 5 edits
        if session.baseline_tokens_per_edit.is_none() && session.edit_event_count >= 5 {
            session.baseline_tokens_per_edit =
                Some(session.total_tokens_at_edit_count as f64 / session.edit_event_count as f64);
        }
    }

    // Track file reads for repetition detection
    if matches!(tool_name, "Read" | "Grep" | "Glob") {
        if let Some(path) = input.get("file_path").and_then(|p| p.as_str()) {
            *session
                .file_reads_since_edit
                .entry(path.to_string())
                .or_insert(0) += 1;
        }
    }
}

pub fn shorten_model(model: &str) -> String {
    models::shorten_model(model)
}

fn refresh_subagent_rollups(session: &mut ClaudeSession) -> UsageRollup {
    // Read *every* discovered subagent (active + completed), so a sub-agent that
    // started and finished between ticks is still fully attributed to the parent.
    for path in session.subagent_jsonl_paths.clone() {
        let rollup = session.subagent_rollups.entry(path.clone()).or_default();
        update_subagent_rollup(&path, rollup, &session.model);
    }

    let mut totals = UsageRollup::default();
    for rollup in session.subagent_rollups.values() {
        totals.input_tokens += rollup.input_tokens;
        totals.output_tokens += rollup.output_tokens;
        totals.cache_read_tokens += rollup.cache_read_tokens;
        totals.cache_write_tokens += rollup.cache_write_tokens;
        totals.cost_usd += rollup.cost_usd;
        totals.usage_metrics_available |= rollup.usage_metrics_available;
        totals.cost_estimate_unverified |= rollup.cost_estimate_unverified;
    }
    totals
}

fn update_subagent_rollup(
    path: &std::path::Path,
    rollup: &mut SubagentRollup,
    default_model: &str,
) {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(_) => return,
    };

    let file_len = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    if rollup.jsonl_offset > file_len {
        *rollup = SubagentRollup::default();
    }

    // Resolve the display label once from the `agent-*.meta.json` sidecar
    // (preserved across the default-reset above, which clears `label`).
    if rollup.label.is_empty() {
        if let Some(label) = resolve_subagent_label(path) {
            rollup.label = label;
        }
    }

    if rollup.jsonl_offset >= file_len {
        rollup.jsonl_offset = file_len;
        return;
    }

    if rollup.jsonl_offset > 0 && file.seek(SeekFrom::Start(rollup.jsonl_offset)).is_err() {
        return;
    }

    let mut current_model = if rollup.model.is_empty() {
        default_model.to_string()
    } else {
        rollup.model.clone()
    };

    let reader = BufReader::new(&file);
    for line in reader.lines() {
        let Ok(line) = line else {
            break;
        };
        let Some(TranscriptEvent::Message(message)) = parse_line(&line) else {
            continue;
        };

        if let Some(ref model) = message.model {
            current_model = shorten_model(model);
            rollup.model = current_model.clone();
        }

        let dedup = dedup_key(message.id.as_deref(), message.request_id.as_deref());
        let Some(usage) = message.usage else {
            continue;
        };

        // Same streaming-duplicate dedup as the parent path: account only the increase.
        let add = merge_usage(&mut rollup.usage_seen, dedup.as_deref(), &usage);
        rollup.input_tokens += add.input;
        rollup.output_tokens += add.output;
        rollup.cache_read_tokens += add.cache_read;
        rollup.cache_write_tokens += add.cache_write;
        rollup.usage_metrics_available = true;

        let input_with_cache = add.input + add.cache_read + add.cache_write;
        let model_for_cost = if current_model.is_empty() {
            default_model
        } else {
            current_model.as_str()
        };
        let (delta_cost, unverified) = estimate_cost_components(
            model_for_cost,
            input_with_cache,
            add.output,
            add.cache_read,
            add.cache_write,
        );
        rollup.cost_usd += delta_cost;
        rollup.cost_estimate_unverified |= unverified;
    }

    rollup.jsonl_offset = file_len;
}

/// Longest a subagent row label may be before we trim it (the roster Project cell
/// is narrow and already indented two levels under the agent).
const SUBAGENT_LABEL_MAX: usize = 44;

/// Build a human label for a subagent from its `agent-*.meta.json` sidecar:
/// `"{agentType} · {description}"` (e.g. "Explore · Map launch→discovery"), trimmed
/// to [`SUBAGENT_LABEL_MAX`]. Returns `None` when the sidecar is missing/unparseable
/// (caller then falls back to the file stem).
fn resolve_subagent_label(jsonl_path: &std::path::Path) -> Option<String> {
    let meta_path = jsonl_path.with_extension("meta.json");
    let content = std::fs::read_to_string(&meta_path).ok()?;
    let value: Value = serde_json::from_str(&content).ok()?;

    let agent_type = value.get("agentType").and_then(|v| v.as_str()).unwrap_or("");
    let description = value
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let label = match (agent_type.is_empty(), description.is_empty()) {
        (false, false) => format!("{agent_type} \u{00b7} {description}"),
        (false, true) => agent_type.to_string(),
        (true, false) => description.to_string(),
        (true, true) => return None,
    };

    Some(truncate_label(&label, SUBAGENT_LABEL_MAX))
}

/// Trim a label to `max` chars on a char boundary, appending an ellipsis.
fn truncate_label(label: &str, max: usize) -> String {
    if label.chars().count() <= max {
        return label.to_string();
    }
    let kept: String = label.chars().take(max.saturating_sub(1)).collect();
    format!("{kept}\u{2026}")
}

fn estimate_cost_components(
    model: &str,
    total_input_tokens: u64,
    total_output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
) -> (f64, bool) {
    let plain_input = total_input_tokens
        .saturating_sub(cache_read_tokens)
        .saturating_sub(cache_write_tokens);
    let resolved = models::resolve(model);

    let cost = (plain_input as f64 / 1_000_000.0) * resolved.profile.input_per_m
        + (total_output_tokens as f64 / 1_000_000.0) * resolved.profile.output_per_m
        + (cache_read_tokens as f64 / 1_000_000.0) * resolved.profile.cache_read_per_m
        + (cache_write_tokens as f64 / 1_000_000.0) * resolved.profile.cache_write_per_m;

    (
        cost,
        resolved.source == models::ModelProfileSource::Fallback,
    )
}

#[cfg(test)]
mod tests {
    use super::{infer_status, summarize_tool, update_tokens};
    use crate::session::{ClaudeSession, RawSession, SessionStatus};
    use serde_json::json;
    use std::path::PathBuf;

    fn now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    }

    #[test]
    fn truncate_label_keeps_short_and_ellipsizes_long() {
        use super::truncate_label;
        assert_eq!(truncate_label("Explore", 44), "Explore");
        let long = "Explore \u{00b7} ".to_string() + &"x".repeat(80);
        let out = truncate_label(&long, 44);
        assert_eq!(out.chars().count(), 44);
        assert!(out.ends_with('\u{2026}'));
    }

    #[test]
    fn resolve_subagent_label_reads_meta_sidecar() {
        use super::resolve_subagent_label;
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let jsonl = dir.path().join("agent-abc.jsonl");
        std::fs::File::create(&jsonl).unwrap();
        std::fs::File::create(dir.path().join("agent-abc.meta.json"))
            .unwrap()
            .write_all(br#"{"agentType":"Explore","description":"Map launch flow","toolUseId":"t"}"#)
            .unwrap();

        assert_eq!(
            resolve_subagent_label(&jsonl).as_deref(),
            Some("Explore \u{00b7} Map launch flow")
        );

        // No sidecar → None (caller falls back to the file stem).
        let orphan = dir.path().join("agent-xyz.jsonl");
        std::fs::File::create(&orphan).unwrap();
        assert_eq!(resolve_subagent_label(&orphan), None);
    }

    #[test]
    fn merge_usage_dedups_streaming_duplicates() {
        use super::{dedup_key, merge_usage};
        use crate::session::SeenUsage;
        use crate::transcript::TranscriptUsage;
        use std::collections::HashMap;

        let mut seen: HashMap<String, SeenUsage> = HashMap::new();
        let key = dedup_key(Some("msg_1"), Some("req_1"));
        assert_eq!(key.as_deref(), Some("msg_1:req_1"));

        let u = TranscriptUsage {
            input_tokens: 100,
            cache_read_input_tokens: 50,
            cache_creation_input_tokens: 10,
            output_tokens: 20,
        };
        // First emission counts in full.
        let a = merge_usage(&mut seen, key.as_deref(), &u);
        assert_eq!((a.input, a.cache_read, a.cache_write, a.output), (100, 50, 10, 20));
        // Identical re-emission (streaming duplicate) adds nothing.
        let b = merge_usage(&mut seen, key.as_deref(), &u);
        assert_eq!((b.input, b.cache_read, b.cache_write, b.output), (0, 0, 0, 0));
        // A grown re-emission adds only the increase.
        let u2 = TranscriptUsage {
            output_tokens: 35,
            ..u
        };
        let c = merge_usage(&mut seen, key.as_deref(), &u2);
        assert_eq!((c.input, c.cache_read, c.cache_write, c.output), (0, 0, 0, 15));
    }

    #[test]
    fn merge_usage_keyless_lines_add_in_full() {
        use super::merge_usage;
        use crate::session::SeenUsage;
        use crate::transcript::TranscriptUsage;
        use std::collections::HashMap;

        let mut seen: HashMap<String, SeenUsage> = HashMap::new();
        let u = TranscriptUsage {
            input_tokens: 10,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            output_tokens: 5,
        };
        // No id → can't dedup → both lines count.
        let a = merge_usage(&mut seen, None, &u);
        let b = merge_usage(&mut seen, None, &u);
        assert_eq!(a.input + b.input, 20);
        assert!(seen.is_empty());
    }

    fn idle_session() -> ClaudeSession {
        let mut s = ClaudeSession::from_raw(RawSession {
            pid: 1,
            session_id: "x".into(),
            cwd: "/tmp".into(),
            started_at: 0,
        });
        s.telemetry_status = crate::session::TelemetryStatus::Available;
        s
    }

    #[test]
    fn aged_user_tool_result_with_no_cpu_reads_as_waiting_not_processing() {
        // Regression: a Claude Code agent routinely ends a turn on a tool call,
        // so the transcript parks at a `user`/tool_result line. Once CPU drops and
        // the grace window passes, that must read as Waiting — not the old
        // permanent "Processing at $0/hr".
        let mut s = idle_session();
        s.cpu_percent = 0.0;
        s.last_message_ts = now_ms().saturating_sub(60_000); // 60s ago
        infer_status(&mut s, "user", "", false, false);
        assert_eq!(s.status, SessionStatus::WaitingInput);
    }

    #[test]
    fn fresh_user_tool_result_still_reads_as_processing() {
        // Within the grace window the model is just between a tool result and its
        // next streamed message — keep calling that Processing.
        let mut s = idle_session();
        s.cpu_percent = 0.0;
        s.last_message_ts = now_ms(); // just now
        infer_status(&mut s, "user", "", false, false);
        assert_eq!(s.status, SessionStatus::Processing);
    }

    #[test]
    fn busy_user_tool_result_reads_as_processing_regardless_of_age() {
        // Genuine CPU usage at a user line means it's working, even if old.
        let mut s = idle_session();
        s.cpu_percent = 12.0;
        s.last_message_ts = now_ms().saturating_sub(120_000);
        infer_status(&mut s, "user", "", false, false);
        assert_eq!(s.status, SessionStatus::Processing);
    }

    #[test]
    fn long_parked_user_tool_result_reads_as_idle() {
        let mut s = idle_session();
        s.cpu_percent = 0.0;
        s.last_message_ts = now_ms().saturating_sub(20 * 60_000); // 20 min ago
        infer_status(&mut s, "user", "", false, false);
        assert_eq!(s.status, SessionStatus::Idle);
    }

    #[test]
    fn assistant_end_turn_reads_as_job_done_not_waiting() {
        // BACKLOG "Split waiting states": an assistant end_turn means the task is
        // complete and it's the user's move → JobDone, distinct from API-wait.
        let mut s = idle_session();
        s.cpu_percent = 0.0;
        s.last_message_ts = now_ms(); // just finished
        infer_status(&mut s, "assistant", "end_turn", false, false);
        assert_eq!(s.status, SessionStatus::JobDone);
    }

    #[test]
    fn fresh_pending_tool_reads_as_processing_not_needs_input() {
        // BACKLOG "Needs Input then it changes to processing": a just-called tool
        // (auto-approved, still executing) must NOT flash NeedsInput. Idle CPU +
        // an outstanding tool within the round-trip window stays Processing.
        let mut s = idle_session();
        s.cpu_percent = 0.0;
        s.pending_tool_name = Some("Bash".into());
        s.last_message_ts = now_ms().saturating_sub(2_000); // 2s ago, within window
        infer_status(&mut s, "assistant", "tool_use", false, false);
        assert_eq!(s.status, SessionStatus::Processing);
    }

    #[test]
    fn aged_pending_tool_reads_as_needs_input() {
        // A tool that has sat outstanding past a normal round-trip with the agent
        // idle is a genuine permission prompt → NeedsInput.
        let mut s = idle_session();
        s.cpu_percent = 0.0;
        s.pending_tool_name = Some("Bash".into());
        s.last_message_ts = now_ms().saturating_sub(30_000); // 30s ago
        infer_status(&mut s, "assistant", "tool_use", false, false);
        assert_eq!(s.status, SessionStatus::NeedsInput);
    }

    #[test]
    fn busy_pending_tool_reads_as_processing() {
        // Real CPU at an outstanding tool means it's working, regardless of age.
        let mut s = idle_session();
        s.cpu_percent = 12.0;
        s.pending_tool_name = Some("Bash".into());
        s.last_message_ts = now_ms().saturating_sub(30_000);
        infer_status(&mut s, "assistant", "tool_use", false, false);
        assert_eq!(s.status, SessionStatus::Processing);
    }

    #[test]
    fn api_error_flag_reads_as_error_when_idle() {
        // BACKLOG "Error Display": a sticky API error with low CPU surfaces Error.
        let mut s = idle_session();
        s.cpu_percent = 0.0;
        infer_status(&mut s, "assistant", "end_turn", false, true);
        assert_eq!(s.status, SessionStatus::Error);
    }

    #[test]
    fn active_retry_after_error_reads_as_processing() {
        // If CPU is high (auto-retry in flight), prefer Processing over a stale Error.
        let mut s = idle_session();
        s.cpu_percent = 40.0;
        infer_status(&mut s, "assistant", "end_turn", false, true);
        assert_eq!(s.status, SessionStatus::Processing);
    }

    #[test]
    fn update_tokens_retains_conversation_from_transcript() {
        // End-to-end (Phase 4a): a real transcript line populates the chat buffer.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let line = include_str!("../../../tests/fixtures/real-transcript-line.json");
        std::fs::write(&path, format!("{}\n", line.trim())).unwrap();

        let mut s = ClaudeSession::from_raw(RawSession {
            pid: 1,
            session_id: "x".into(),
            cwd: "/tmp".into(),
            started_at: 0,
        });
        s.jsonl_path = Some(PathBuf::from(&path));

        update_tokens(&mut s);

        assert!(
            !s.conversation.is_empty(),
            "conversation should be populated from the transcript"
        );
    }

    #[test]
    fn summarize_tool_uses_key_arg_then_falls_back_to_name() {
        assert_eq!(
            summarize_tool("Edit", &json!({"file_path": "src/lib.rs"})),
            "Edit: src/lib.rs"
        );
        assert_eq!(
            summarize_tool("Bash", &json!({"command": "cargo test"})),
            "Bash: cargo test"
        );
        // No recognised arg → bare tool name.
        assert_eq!(summarize_tool("TodoWrite", &json!({"todos": []})), "TodoWrite");
    }
}
