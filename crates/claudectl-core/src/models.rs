use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelProfile {
    pub input_per_m: f64,
    pub output_per_m: f64,
    pub cache_read_per_m: f64,
    pub cache_write_per_m: f64,
    pub context_max: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelOverride {
    pub name: String,
    pub profile: ModelProfile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelProfileSource {
    BuiltIn,
    Override,
    Fallback,
}

impl ModelProfileSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::BuiltIn => "built-in",
            Self::Override => "override",
            Self::Fallback => "fallback",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedModelProfile {
    pub key: String,
    pub profile: ModelProfile,
    pub source: ModelProfileSource,
}

static MODEL_OVERRIDES: OnceLock<Mutex<HashMap<String, ModelProfile>>> = OnceLock::new();

/// Normalize a raw model id (e.g. `claude-opus-4-8-20260101`, `claude-opus-4-8[1m]`)
/// to a short `family-major.minor` display key (`opus-4.8`). The family prefix is what
/// [`built_in_profile`] matches on, so any version/date/`[1m]` suffix still prices
/// correctly; the version is kept only for display. Unknown families pass through
/// (lowercased) so config overrides keyed on the full id still resolve.
pub fn shorten_model(model: &str) -> String {
    let lower = model.trim().to_lowercase();
    let family = if lower.contains("opus") {
        "opus"
    } else if lower.contains("sonnet") {
        "sonnet"
    } else if lower.contains("haiku") {
        "haiku"
    } else {
        return lower;
    };

    // Split on every non-alphanumeric boundary so date stamps (`-20260101`) and
    // bracketed suffixes (`[1m]`) don't confuse the version scan, then take the two
    // numeric segments immediately after the family word.
    let tokens: Vec<&str> = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    if let Some(i) = tokens.iter().position(|t| *t == family)
        && let (Some(maj), Some(min)) = (tokens.get(i + 1), tokens.get(i + 2))
        && let (Ok(maj), Ok(min)) = (maj.parse::<u32>(), min.parse::<u32>())
        && maj < 100
        && min < 100
    {
        return format!("{family}-{maj}.{min}");
    }
    family.to_string()
}

pub fn set_overrides(overrides: Vec<ModelOverride>) {
    let store = MODEL_OVERRIDES.get_or_init(|| Mutex::new(HashMap::new()));
    let Ok(mut guard) = store.lock() else {
        return;
    };
    guard.clear();
    for override_ in overrides {
        let raw = override_.name.trim().to_lowercase();
        let shortened = shorten_model(&override_.name).to_lowercase();
        guard.insert(raw, override_.profile);
        guard.insert(shortened, override_.profile);
    }
}

pub fn resolve(model: &str) -> ResolvedModelProfile {
    let empty = HashMap::new();
    let store = MODEL_OVERRIDES.get_or_init(|| Mutex::new(HashMap::new()));
    let guard = store.lock().ok();
    let overrides = guard.as_deref().unwrap_or(&empty);
    resolve_with_overrides(model, overrides)
}

pub(crate) fn resolve_with_overrides(
    model: &str,
    overrides: &HashMap<String, ModelProfile>,
) -> ResolvedModelProfile {
    let raw_key = model.trim().to_lowercase();
    let short_key = shorten_model(model).to_lowercase();

    if let Some(profile) = overrides
        .get(&raw_key)
        .or_else(|| overrides.get(&short_key))
        .copied()
    {
        return ResolvedModelProfile {
            key: if raw_key.is_empty() {
                short_key
            } else {
                raw_key
            },
            profile,
            source: ModelProfileSource::Override,
        };
    }

    if let Some(profile) = built_in_profile(&short_key) {
        return ResolvedModelProfile {
            key: short_key,
            profile,
            source: ModelProfileSource::BuiltIn,
        };
    }

    ResolvedModelProfile {
        key: if short_key.is_empty() {
            "unknown".into()
        } else {
            short_key
        },
        profile: fallback_profile(),
        source: ModelProfileSource::Fallback,
    }
}

/// Built-in per-1M-token pricing, matched by model *family* prefix so every version
/// (4.5/4.6/4.7/4.8, date-stamped, `[1m]`) resolves. Current Anthropic list pricing
/// (verified against the `claude-api` reference, 2026-06):
///   Opus 4.5–4.8: $5 in / $25 out · Sonnet 4–4.6: $3 / $15 · Haiku 4.5: $1 / $5
/// Cache follows the standard formula: read = 0.1× input, 5-minute write = 1.25× input.
/// (Earlier the table priced `opus` at $15/$75 — old Opus-4.0/4.1 numbers — over-charging
/// Opus 4.5+ by ~3×; see `docs/COMPARABLES.md`.)
fn built_in_profile(key: &str) -> Option<ModelProfile> {
    if key.starts_with("opus") {
        Some(ModelProfile {
            input_per_m: 5.0,
            output_per_m: 25.0,
            cache_read_per_m: 0.5,
            cache_write_per_m: 6.25,
            context_max: 1_000_000,
        })
    } else if key.starts_with("sonnet") {
        Some(ModelProfile {
            input_per_m: 3.0,
            output_per_m: 15.0,
            cache_read_per_m: 0.30,
            cache_write_per_m: 3.75,
            context_max: 1_000_000,
        })
    } else if key.starts_with("haiku") {
        Some(ModelProfile {
            input_per_m: 1.0,
            output_per_m: 5.0,
            cache_read_per_m: 0.10,
            cache_write_per_m: 1.25,
            context_max: 200_000,
        })
    } else {
        None
    }
}

/// Unknown model: price conservatively at mid-tier (Sonnet) rather than the old Opus
/// default. Sessions on the fallback are flagged `cost_estimate_unverified` via
/// [`ModelProfileSource::Fallback`].
fn fallback_profile() -> ModelProfile {
    ModelProfile {
        input_per_m: 3.0,
        output_per_m: 15.0,
        cache_read_per_m: 0.30,
        cache_write_per_m: 3.75,
        context_max: 200_000,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_builtin_profile() {
        let resolved = resolve_with_overrides("claude-opus-4-6-20260401", &HashMap::new());
        assert_eq!(resolved.source, ModelProfileSource::BuiltIn);
        assert_eq!(resolved.profile.context_max, 1_000_000);
    }

    #[test]
    fn resolve_override_profile() {
        let mut overrides = HashMap::new();
        overrides.insert(
            "gpt-4o".into(),
            ModelProfile {
                input_per_m: 1.0,
                output_per_m: 2.0,
                cache_read_per_m: 0.5,
                cache_write_per_m: 1.5,
                context_max: 128_000,
            },
        );
        let resolved = resolve_with_overrides("gpt-4o", &overrides);
        assert_eq!(resolved.source, ModelProfileSource::Override);
        assert_eq!(resolved.profile.context_max, 128_000);
    }

    #[test]
    fn resolve_fallback_profile() {
        let resolved = resolve_with_overrides("mystery-model", &HashMap::new());
        assert_eq!(resolved.source, ModelProfileSource::Fallback);
        assert_eq!(resolved.profile.context_max, 200_000);
    }

    #[test]
    fn opus_48_priced_at_current_rate_not_legacy() {
        // Regression for the ~3× over-price: Opus 4.5–4.8 is $5/$25, not the old
        // Opus-4.0/4.1 $15/$75 (docs/COMPARABLES.md).
        for id in [
            "claude-opus-4-8",
            "claude-opus-4-8-20260101",
            "claude-opus-4-8[1m]",
            "claude-opus-4-5-20251101",
        ] {
            let r = resolve_with_overrides(id, &HashMap::new());
            assert_eq!(r.source, ModelProfileSource::BuiltIn, "{id}");
            assert_eq!(r.profile.input_per_m, 5.0, "{id}");
            assert_eq!(r.profile.output_per_m, 25.0, "{id}");
        }
    }

    #[test]
    fn shorten_model_extracts_family_and_version() {
        assert_eq!(shorten_model("claude-opus-4-8-20260101"), "opus-4.8");
        assert_eq!(shorten_model("claude-opus-4-8[1m]"), "opus-4.8");
        assert_eq!(shorten_model("claude-sonnet-4-6-20260401"), "sonnet-4.6");
        assert_eq!(shorten_model("claude-haiku-4-5"), "haiku-4.5");
        // Legacy name-after-version layout: version not adjacent → family only.
        assert_eq!(shorten_model("claude-3-5-haiku-20241022"), "haiku");
        // Unknown family passes through (lowercased) so overrides still key on it.
        assert_eq!(shorten_model("GPT-4o"), "gpt-4o");
    }
}
