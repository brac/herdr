---
name: herdr-cost
description: How herdr prices tokens and accounts usage — the per-model pricing table, the family/version model matcher, the streaming-duplicate dedup, and where every cost number comes from. Use for any pricing, token-count, "cost/$ looks wrong", or model-name-resolution work. For status/CPU and the $/hr burn rate itself, see herdr-status.
---

# herdr-cost — token accounting, pricing, and dedup

Cost is a headline number and easy to get subtly wrong (a wrong pricing row over-charged
Opus 4.8 by ~3x; un-deduped streaming lines over-counted tokens). This is the model and the traps.

## Where it lives

- **`core/models.rs`** — the per-model pricing table (`built_in_profile`), the model-name matcher
  (`shorten_model`), resolution (`resolve` / `resolve_with_overrides`), and the `set_overrides` seam.
- **`core/transcript.rs`** — `TranscriptUsage` + `parse_usage` (the four token tiers), and
  `message.id` / top-level `requestId` (the dedup key).
- **`core/monitor.rs`** — `update_tokens` (incremental JSONL accumulation, own/parent path),
  `merge_usage` + `dedup_key` (the streaming-dup dedup), `estimate_cost_components` (the per-tier
  multiply), `finalize_usage` (rolls own + subagent into `session.cost_usd`), and
  `update_subagent_rollup` (the subagent path).
- **`core/session.rs`** — the token fields on `ClaudeSession` (`own_*`, `subagent_*`, `total_*`,
  `cache_*`), `SeenUsage` + the `usage_seen` dedup maps, and `SubagentRollup`.
- **`core/history.rs`** — persisted per-session CSV (`record_session` on first Finished), the
  weekly/all-time summaries, and the daily activity series (`daily_cost_series` / `intensities`).

## The pricing model

- **Table is matched by model FAMILY PREFIX**, not exact id. `shorten_model` turns a raw id
  (`claude-opus-4-8-20260101`, `claude-opus-4-8[1m]`) into `family-major.minor` (`opus-4.8`) by
  splitting on non-alphanumerics; `built_in_profile` then matches `starts_with("opus"|"sonnet"|"haiku")`,
  so every version/date/`[1m]` suffix resolves to the right family.
- **Current rates** (per 1M tokens, verify against the `claude-api` skill before changing):
  Opus 4.5–4.8 `$5 / $25`, Sonnet 4–4.6 `$3 / $15`, Haiku 4.5 `$1 / $5`. Cache follows the standard
  formula: read = 0.1x input, 5-minute write = 1.25x input.
- **Resolution order** (`resolve_with_overrides`): config override (raw key, then short key) →
  built-in family table → fallback. The fallback is mid-tier (Sonnet) and flags
  `cost_estimate_unverified` via `ModelProfileSource::Fallback`.
- **To change a price:** edit `built_in_profile` in `models.rs`. Do NOT trust recalled numbers; pull
  current pricing from the `claude-api` skill / Anthropic docs.

## Streaming-duplicate dedup (load-bearing)

Claude Code writes the SAME assistant message's `usage` on several physical JSONL lines as it
streams. Naive accumulation over-counts. herdr dedups:

- Key = `message.id:requestId` (`dedup_key`). `merge_usage` keeps the per-tier max seen for that key
  and adds only the *increase* to the running totals. Keyless lines (no id) add in full.
- Applied on BOTH paths: own (`update_tokens`) and subagent (`update_subagent_rollup`), each with its
  own `usage_seen` map.
- State persists across incremental ticks (carried by `tui/app.rs::merge_discovered_session` when the
  session id is unchanged), and resets for free on `/clear` (fresh session) and on file truncation
  (`usage_seen.clear()`).

## Gotchas that have each cost real money / a session

- **Own path prices from cumulative totals; subagent path accumulates per-message.** They agree today
  ONLY because pricing is linear. Anyone adding a non-linear tier (e.g. a >200k premium) must move the
  own path to per-message cost accumulation first, or the sum will be mis-tiered.
- **No >200k long-context tier — by design.** Current Opus/Sonnet 1M context is flat-priced (no
  premium), so a marginal-above-200k tier would OVER-charge. Do not add one for these models.
- **`costUSD` is only sporadically present** in transcripts. With the table correct, computed cost
  already matches it, so trusting `costUSD` ("CostMode::Auto") is low value and was deferred.
- **The 5m/1h cache split is real but minor.** `usage.cache_creation` carries
  `ephemeral_{5m,1h}_input_tokens`; herdr currently prices all cache-creation at the 5m rate. ~1.6x on
  a small tier; deferred.
- `own_input_tokens` already INCLUDES cache (input + cache_read + cache_create); `estimate_cost_components`
  subtracts cache back out to get plain input. `SubagentRollup.input_tokens` is plain input. Keep that
  straight when touching either path.

## Verify

- Unit tests: `models::tests::opus_48_priced_at_current_rate_not_legacy`,
  `shorten_model_extracts_family_and_version`, `monitor::tests::merge_usage_*`.
- Live: run herdr on an Opus-4.8 agent and compare its per-session cost against `npx ccusage` /
  the local `../ccusage` for the same session id; they should agree within rounding.

## Reference

Technique provenance is in `docs/COMPARABLES.md` (ccusage's pricing table + matcher; tokscale's
dedup/max-merge). The cloned tools are siblings: `../ccusage`, `../tokscale`.
