//! Backend-neutral quota view: every usage window an upstream reports, in one shape.
//!
//! The parsing functions are pure; the clock is passed in.
use crate::resets::{ExtraLimit, RateLimit, UsageSnapshot, Window};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Mutex;

/// A quota view older than this is flagged stale. Disabled Claude accounts are polled every
/// ten minutes, so the threshold must exceed that.
pub const STALE_AFTER_MS: i64 = 20 * 60 * 1000;

/// How often a disabled account's quota is refreshed for display.
pub const DISABLED_POLL_MS: i64 = 10 * 60 * 1000;

/// Whether a disabled account is due for a display-only quota poll.
pub fn disabled_poll_due(last_poll_ms: Option<i64>, now_ms: i64) -> bool {
    last_poll_ms.is_none_or(|last| now_ms.saturating_sub(last) >= DISABLED_POLL_MS)
}

/// One usage window.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct QuotaWindow {
    pub key: String,
    pub label: String,
    /// Fraction of the window consumed, clamped to 0.0..=1.0.
    pub used: f64,
    pub resets_at_ms: Option<i64>,
    pub window_secs: Option<i64>,
}

/// Every window of one account plus its banked resets.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct QuotaView {
    pub windows: Vec<QuotaWindow>,
    pub banked_resets: Option<i64>,
    pub credit_expirations: Vec<i64>,
    pub fetched_at_ms: i64,
    pub stale: bool,
}

impl QuotaWindow {
    /// Per-model limits (`limit:` keys) are display-only; only the other windows gate an account.
    pub fn is_account_wide(&self) -> bool {
        !self.key.starts_with("limit:")
    }
}

impl QuotaView {
    /// The same view with `stale` evaluated at `now_ms`.
    pub fn at(mut self, now_ms: i64) -> Self {
        self.stale = now_ms.saturating_sub(self.fetched_at_ms) > STALE_AFTER_MS;
        self
    }
}

fn fraction(percent: f64) -> f64 {
    if percent.is_finite() {
        (percent / 100.0).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// A Codex window length as a short label: 18000 -> "5h", 604800 -> "7d".
fn codex_length_label(seconds: i64) -> String {
    if seconds >= 86_400 && seconds % 86_400 == 0 {
        format!("{}d", seconds / 86_400)
    } else if seconds >= 3_600 && seconds % 3_600 == 0 {
        format!("{}h", seconds / 3_600)
    } else if seconds >= 3_600 {
        format!("{:.1}h", seconds as f64 / 3_600.0)
    } else {
        format!("{}m", seconds / 60)
    }
}

fn codex_window(prefix: Option<&str>, slot: &str, window: &Window) -> QuotaWindow {
    let length = window
        .limit_window_seconds
        .filter(|s| *s > 0)
        .map_or_else(|| slot.to_string(), codex_length_label);
    let (key, label) = match prefix {
        Some(name) => (format!("{name}:{slot}"), format!("{name} {length}")),
        None => (slot.to_string(), length),
    };
    QuotaWindow {
        key,
        label,
        used: fraction(window.used_percent),
        resets_at_ms: window.reset_at.and_then(|s| s.checked_mul(1000)),
        window_secs: window.limit_window_seconds,
    }
}

fn push_rate_limit(out: &mut Vec<QuotaWindow>, prefix: Option<&str>, limit: &RateLimit) {
    if let Some(w) = &limit.primary_window {
        out.push(codex_window(prefix, "primary", w));
    }
    if let Some(w) = &limit.secondary_window {
        out.push(codex_window(prefix, "secondary", w));
    }
}

/// Parses the extra limit buckets of a `/wham/usage` payload (`additional_rate_limits`),
/// tolerating an array of buckets or a map of name to bucket. Unknown shapes yield nothing.
pub fn parse_additional_limits(value: &Value) -> Vec<ExtraLimit> {
    let bucket = |fallback: Option<&str>, item: &Value| -> Option<ExtraLimit> {
        let name = ["limit_name", "metered_feature", "name"]
            .iter()
            .find_map(|k| item.get(*k).and_then(Value::as_str))
            .or(fallback)?
            .to_string();
        let source = item
            .get("rate_limit")
            .filter(|v| v.is_object())
            .unwrap_or(item);
        let rate_limit: RateLimit = serde_json::from_value(source.clone()).ok()?;
        if rate_limit.primary_window.is_none() && rate_limit.secondary_window.is_none() {
            return None;
        }
        Some(ExtraLimit { name, rate_limit })
    };
    match value {
        Value::Array(items) => items.iter().filter_map(|i| bucket(None, i)).collect(),
        Value::Object(map) => map
            .iter()
            .filter_map(|(k, i)| bucket(Some(k.as_str()), i))
            .collect(),
        _ => Vec::new(),
    }
}

/// @cc [owner:edwrdc,label:pool] quota-from-codex-snapshot
/// `from_codex` MUST report the primary and secondary windows of the account limit followed by
/// those of every additional limit bucket (labelled with the bucket name), with `used` as a
/// fraction clamped to 0..=1, reset times in epoch milliseconds, and `banked_resets` and
/// `credit_expirations` taken from the snapshot. `stale` MUST be computed from the passed clock.
pub fn from_codex(snapshot: &UsageSnapshot, now_ms: i64) -> QuotaView {
    let mut windows = Vec::new();
    if let Some(limit) = &snapshot.rate_limit {
        push_rate_limit(&mut windows, None, limit);
    }
    for extra in &snapshot.additional_rate_limits {
        push_rate_limit(&mut windows, Some(&extra.name), &extra.rate_limit);
    }
    QuotaView {
        windows,
        banked_resets: Some(snapshot.available_resets),
        credit_expirations: snapshot
            .credit_expirations
            .iter()
            .flatten()
            .copied()
            .collect(),
        fetched_at_ms: snapshot.fetched_at_ms,
        stale: false,
    }
    .at(now_ms)
}

/// The display label of a Claude usage window key.
fn claude_label(key: &str) -> String {
    let scoped = |base: &str, short: &str| {
        let rest = key.strip_prefix(base)?;
        if rest.is_empty() {
            Some(short.to_string())
        } else {
            rest.strip_prefix('_')
                .map(|r| format!("{short} {}", r.replace('_', " ")))
        }
    };
    scoped("five_hour", "5h")
        .or_else(|| scoped("seven_day", "7d"))
        .unwrap_or_else(|| key.to_string())
}

fn claude_group(key: &str) -> u8 {
    if key.starts_with("five_hour") {
        0
    } else if key.starts_with("seven_day") {
        1
    } else if key.starts_with("limit:weekly") {
        2
    } else {
        3
    }
}

/// One scoped window from an entry of the `limits` array, or `None` for the session and
/// all-models entries (which duplicate the top-level `five_hour` and `seven_day`).
fn claude_limit_window(entry: &Value) -> Option<QuotaWindow> {
    let kind = entry.get("kind")?.as_str()?;
    if kind == "session" || kind == "weekly_all" {
        return None;
    }
    let group = entry.get("group").and_then(Value::as_str).unwrap_or(kind);
    let scope = entry.get("scope");
    let text = |path: &[&str]| -> Option<String> {
        let mut node = scope?;
        for step in path {
            node = node.get(*step)?;
        }
        node.as_str().filter(|s| !s.is_empty()).map(str::to_string)
    };
    let name = text(&["model", "display_name"])
        .or_else(|| text(&["model", "id"]))
        .or_else(|| text(&["surface"]))
        .unwrap_or_else(|| kind.to_string());
    let label = match group {
        "weekly" => format!("7d {name}"),
        "session" => format!("5h {name}"),
        other => format!("{other} {name}"),
    };
    let resets_at_ms = entry
        .get("resets_at")
        .and_then(Value::as_str)
        .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok())
        .map(|v| v.timestamp_millis());
    Some(QuotaWindow {
        key: format!("limit:{kind}:{}", name.to_lowercase()),
        label,
        used: fraction(entry.get("percent")?.as_f64()?),
        resets_at_ms,
        window_secs: match group {
            "weekly" => Some(7 * 86_400),
            "session" => Some(5 * 3600),
            _ => None,
        },
    })
}

/// The windows of an `/api/oauth/usage` payload: every top-level object with a numeric
/// `utilization`, ordered 5h-ish, then 7d-ish, then the rest alphabetically.
pub fn claude_windows(body: &Value) -> Vec<QuotaWindow> {
    let Some(map) = body.as_object() else {
        return Vec::new();
    };
    let scoped: Vec<QuotaWindow> = body
        .get("limits")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(claude_limit_window).collect())
        .unwrap_or_default();
    let mut windows: Vec<QuotaWindow> = map
        .iter()
        .filter(|(key, _)| key.as_str() != "extra_usage" && key.as_str() != "limits")
        .filter_map(|(key, value)| {
            let used = value.get("utilization")?.as_f64()?;
            let resets_at_ms = value
                .get("resets_at")
                .and_then(Value::as_str)
                .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok())
                .map(|v| v.timestamp_millis());
            Some(QuotaWindow {
                key: key.clone(),
                label: claude_label(key),
                used: fraction(used),
                resets_at_ms,
                window_secs: match claude_group(key) {
                    0 => Some(5 * 3600),
                    1 => Some(7 * 86_400),
                    _ => None,
                },
            })
        })
        .filter(|w| {
            // A `limits` entry is preferred over a top-level key with the same label.
            !scoped
                .iter()
                .any(|s| s.label.eq_ignore_ascii_case(&w.label))
        })
        .collect();
    windows.extend(scoped);
    windows.sort_by(|a, b| {
        claude_group(&a.key)
            .cmp(&claude_group(&b.key))
            .then_with(|| a.key.cmp(&b.key))
    });
    windows
}

/// @cc [owner:edwrdc,label:pool] quota-from-claude-usage
/// `from_claude_usage` MUST turn every top-level key of the usage payload whose value is an
/// object with a numeric `utilization` into exactly one window (percent converted to a 0..=1
/// fraction), and MUST skip null or non-window keys. Ordering MUST be deterministic: five-hour
/// windows, then seven-day windows, then scoped weekly windows, then the rest, each
/// alphabetically by key. The top-level `extra_usage` (spend credits) is never a window. Per-model
/// windows come from the `limits` array: every entry whose kind is not `session` or `weekly_all`
/// (those duplicate `five_hour` and `seven_day`) MUST yield exactly one window, keyed
/// `limit:<kind>:<scope>` and labelled like `7d Fable`, replacing any top-level window with the
/// same label. Scoped windows are display-only and never affect cooling. Banked resets are not
/// in this payload and are left unset for the caller.
pub fn from_claude_usage(body: &Value, now_ms: i64) -> QuotaView {
    QuotaView {
        windows: claude_windows(body),
        banked_resets: None,
        credit_expirations: Vec::new(),
        fetched_at_ms: now_ms,
        stale: false,
    }
}

/// A raw upstream JSON body kept for diagnostics. Bodies never include headers or tokens.
#[derive(Clone, Debug, Serialize)]
pub struct RawPayload {
    pub fetched_at_ms: i64,
    pub body: Value,
}

/// The last raw payload per name, per account. Not part of the pure core: guarded by a mutex.
#[derive(Default)]
pub struct RawPayloads {
    inner: Mutex<BTreeMap<String, BTreeMap<String, RawPayload>>>,
}

impl RawPayloads {
    pub fn record(&self, account_id: &str, name: &str, body: Value, now_ms: i64) {
        self.inner
            .lock()
            .unwrap()
            .entry(account_id.to_string())
            .or_default()
            .insert(
                name.to_string(),
                RawPayload {
                    fetched_at_ms: now_ms,
                    body,
                },
            );
    }

    pub fn get(&self, account_id: &str) -> BTreeMap<String, RawPayload> {
        self.inner
            .lock()
            .unwrap()
            .get(account_id)
            .cloned()
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn claude_payload_yields_generic_windows() {
        let body = json!({
            "plan_type": "max",
            "seven_day_opus": null,
            "seven_day_fable_5_2": {"utilization": 12.5, "resets_at": "2026-10-27T00:00:00Z"},
            "seven_day_fable": {"utilization": 100.0, "resets_at": "2026-10-27T00:00:00Z"},
            "seven_day": {"utilization": 5.0, "resets_at": "2026-10-27T00:00:00Z"},
            "five_hour": {"utilization": 7.0, "resets_at": "2026-10-20T13:00:00Z"},
            "extra_usage": {"utilization": null, "resets_at": null}
        });
        let view = from_claude_usage(&body, 1_000);
        let keys: Vec<_> = view.windows.iter().map(|w| w.key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "five_hour",
                "seven_day",
                "seven_day_fable",
                "seven_day_fable_5_2"
            ]
        );
        let labels: Vec<_> = view.windows.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(labels, ["5h", "7d", "7d fable", "7d fable 5 2"]);
        assert_eq!(view.windows[0].used, 0.07);
        assert_eq!(view.windows[2].used, 1.0);
        assert_eq!(view.windows[0].resets_at_ms, Some(1_792_501_200_000));
    }

    #[test]
    fn real_payload_fixture_yields_scoped_limit_windows() {
        let body: Value =
            serde_json::from_str(include_str!("../tests/fixtures/claude_oauth_usage.json"))
                .unwrap();
        let view = from_claude_usage(&body, 1_000);
        let labels: Vec<_> = view.windows.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(labels, ["5h", "7d", "7d Fable"]);
        assert_eq!(view.windows[0].used, 0.0);
        assert_eq!(view.windows[1].used, 0.25);
        assert_eq!(view.windows[2].used, 0.48);
        assert_eq!(view.windows[2].key, "limit:weekly_scoped:fable");
        assert_eq!(view.windows[2].window_secs, Some(604_800));
        assert!(view.windows[2].resets_at_ms.is_some());
    }

    #[test]
    fn limits_entry_replaces_same_label_top_level_window() {
        let body = json!({
            "seven_day_fable": {"utilization": 10.0, "resets_at": null},
            "limits": [{"group": "weekly", "kind": "weekly_scoped", "percent": 40,
                "resets_at": null, "scope": {"model": {"display_name": "Fable", "id": null}, "surface": null}}]
        });
        let windows = claude_windows(&body);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].used, 0.4);
    }

    #[test]
    fn codex_snapshot_reports_both_windows_and_extras() {
        let window = |used, secs, reset| Window {
            used_percent: used,
            reset_at: Some(reset),
            limit_window_seconds: Some(secs),
        };
        let snapshot = UsageSnapshot {
            rate_limit: Some(RateLimit {
                allowed: Some(true),
                limit_reached: Some(false),
                primary_window: Some(window(20.0, 18_000, 100)),
                secondary_window: Some(window(130.0, 604_800, 200)),
            }),
            additional_rate_limits: vec![ExtraLimit {
                name: "spark".into(),
                rate_limit: RateLimit {
                    primary_window: Some(window(1.0, 18_000, 300)),
                    ..Default::default()
                },
            }],
            available_resets: 1,
            credit_expirations: vec![Some(5), None],
            fetched_at_ms: 1_000,
        };
        let view = from_codex(&snapshot, 1_000 + STALE_AFTER_MS + 1);
        let labels: Vec<_> = view.windows.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(labels, ["5h", "7d", "spark 5h"]);
        assert_eq!(view.windows[0].used, 0.2);
        assert_eq!(view.windows[1].used, 1.0);
        assert_eq!(view.windows[0].resets_at_ms, Some(100_000));
        assert_eq!(view.banked_resets, Some(1));
        assert_eq!(view.credit_expirations, vec![5]);
        assert!(view.stale);
    }

    #[test]
    fn additional_limits_accept_arrays_and_maps() {
        let array = json!([{
            "limit_name": "GPT-5.3-Codex-Spark",
            "rate_limit": {"primary_window": {"used_percent": 3.0, "reset_at": 9, "limit_window_seconds": 18000}}
        }]);
        let found = parse_additional_limits(&array);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "GPT-5.3-Codex-Spark");
        let map = json!({"x": {"primary_window": {"used_percent": 3.0}}});
        assert_eq!(parse_additional_limits(&map)[0].name, "x");
        assert!(parse_additional_limits(&json!("nope")).is_empty());
    }
}
