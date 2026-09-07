//! Primitives shared by every module: canonical JSON, digests, id generation,
//! RFC 3339 handling. See `docs/api.md` §2.3, §2.5.

use serde_json::Value;
use sha2::{Digest, Sha256};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

/// Canonical JSON: object keys sorted by unicode code point, no insignificant
/// whitespace, UTF-8. Implemented explicitly so the result never depends on
/// which `serde_json` features a dependency happens to enable.
pub fn canonical_json(value: &Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_unstable();
            out.push('{');
            for (i, key) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String((*key).clone()).to_string());
                out.push(':');
                write_canonical(&map[*key], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// `"sha256:" + hex(sha256(canonical_json(value)))`.
pub fn digest_json(value: &Value) -> String {
    format!("sha256:{}", sha256_hex(canonical_json(value).as_bytes()))
}

pub fn digest_text(text: &str) -> String {
    format!("sha256:{}", sha256_hex(text.as_bytes()))
}

/// Deterministic server-side id: `<prefix>_<first 16 hex of sha256(seed)>`.
/// Deterministic so that an idempotent retry derives the same ids.
pub fn derived_id(prefix: &str, seed: &str) -> String {
    let hex = sha256_hex(seed.as_bytes());
    format!("{prefix}_{}", &hex[..16])
}

/// Server record time: UTC with millisecond precision.
pub fn now_utc_millis() -> String {
    format_utc_millis(OffsetDateTime::now_utc())
}

pub fn format_utc_millis(ts: OffsetDateTime) -> String {
    let ts = ts.to_offset(time::UtcOffset::UTC);
    // RFC3339's default formatter omits zero fractional digits, making
    // "00Z" sort after "00.001Z". Fixed precision preserves time ordering.
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        ts.year(),
        ts.month() as u8,
        ts.day(),
        ts.hour(),
        ts.minute(),
        ts.second(),
        ts.millisecond()
    )
}

/// Parse an RFC 3339 timestamp. The format requires an explicit offset, so a
/// naive local timestamp is rejected here (`timezone_required`).
pub fn parse_rfc3339(text: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(text, &Rfc3339).ok()
}

/// Normalised UTC string used for scalar comparisons in storage and queries.
pub fn to_utc_key(ts: OffsetDateTime) -> String {
    format_utc_millis(ts.to_offset(time::UtcOffset::UTC))
}

/// Numeric literals in a text, as an ordered multiset used to decide whether an
/// edit is a cosmetic correction or a semantic change. `docs/api.md` §3.3.
pub fn numeric_tokens(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0usize;
    while i < chars.len() {
        if !chars[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = if i > 0 && matches!(chars[i - 1], '-' | '+') {
            i - 1
        } else {
            i
        };
        while i < chars.len() {
            let c = chars[i];
            if c.is_ascii_digit()
                || ((c == '.' || c == ',') && i + 1 < chars.len() && chars[i + 1].is_ascii_digit())
            {
                i += 1;
            } else {
                break;
            }
        }
        let raw: String = chars[start..i].iter().collect();
        tokens.push(normalize_number(&raw));
    }
    tokens.sort();
    tokens
}

/// `1,204` and `1204` are the same quantity; `30.0` and `30` are not
/// distinguished either. Leading zeros are dropped.
fn normalize_number(raw: &str) -> String {
    let sign = if raw.starts_with('-') { "-" } else { "" };
    let without_group: String = raw
        .trim_start_matches(['-', '+'])
        .chars()
        .filter(|c| *c != ',')
        .collect();
    let (int_part, frac_part) = match without_group.split_once('.') {
        Some((a, b)) => (a.to_string(), b.trim_end_matches('0').to_string()),
        None => (without_group, String::new()),
    };
    let int_trimmed = int_part.trim_start_matches('0');
    let int_final = if int_trimmed.is_empty() {
        "0"
    } else {
        int_trimmed
    };
    if frac_part.is_empty() {
        format!("{sign}{int_final}")
    } else {
        format!("{sign}{int_final}.{frac_part}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn canonical_json_sorts_keys_and_is_stable() {
        let a = json!({"b": 1, "a": {"z": [1, 2], "y": null}});
        let b = json!({"a": {"y": null, "z": [1, 2]}, "b": 1});
        assert_eq!(canonical_json(&a), canonical_json(&b));
        assert_eq!(canonical_json(&a), r#"{"a":{"y":null,"z":[1,2]},"b":1}"#);
    }

    #[test]
    fn zero_fraction_sorts_before_the_next_millisecond() {
        let zero = to_utc_key(parse_rfc3339("2026-01-04T09:00:00+09:00").unwrap());
        let one = to_utc_key(parse_rfc3339("2026-01-04T00:00:00.001Z").unwrap());
        assert_eq!(zero, "2026-01-04T00:00:00.000Z");
        assert!(zero < one);
        assert_ne!(numeric_tokens("-3 degrees"), numeric_tokens("3 degrees"));
    }

    #[test]
    fn canonical_json_preserves_array_order() {
        let a = json!([2, 1]);
        let b = json!([1, 2]);
        assert_ne!(canonical_json(&a), canonical_json(&b));
    }

    #[test]
    fn derived_id_is_deterministic() {
        assert_eq!(derived_id("rev", "k:rev_a"), derived_id("rev", "k:rev_a"));
        assert_ne!(derived_id("rev", "k:rev_a"), derived_id("rev", "k:rev_b"));
        assert!(derived_id("rev", "x").starts_with("rev_"));
    }

    #[test]
    fn rfc3339_requires_an_offset() {
        assert!(parse_rfc3339("2026-09-01T10:00:00+09:00").is_some());
        assert!(parse_rfc3339("2026-09-01T01:00:00Z").is_some());
        assert!(parse_rfc3339("2026-09-01T10:00:00").is_none());
        assert!(parse_rfc3339("2026-09-01").is_none());
    }

    #[test]
    fn utc_key_orders_across_offsets() {
        let seoul = parse_rfc3339("2026-09-01T10:00:00+09:00").unwrap();
        let utc = parse_rfc3339("2026-09-01T01:00:00Z").unwrap();
        assert_eq!(to_utc_key(seoul), to_utc_key(utc));
    }

    #[test]
    fn numeric_tokens_detect_behaviour_change() {
        // 3초 -> 30초 is a semantic change, not a typo fix.
        assert_ne!(
            numeric_tokens("타임아웃 3초"),
            numeric_tokens("타임아웃 30초")
        );
        // Pure wording fix keeps the same numbers.
        assert_eq!(
            numeric_tokens("연속 5회 실패시 30분 잠금"),
            numeric_tokens("연속 5회 실패 시 30분 동안 잠근다")
        );
        // Grouping and trailing zeros are not behaviour changes.
        assert_eq!(numeric_tokens("1,204건"), numeric_tokens("1204건"));
        assert_eq!(numeric_tokens("30.0분"), numeric_tokens("30분"));
        // Reordering the same numbers is still cosmetic.
        assert_eq!(numeric_tokens("5회 30분"), numeric_tokens("30분 안에 5회"));
    }
}
