//! Value-pattern PII scrubbing.
//!
//! When `send_default_pii` is `false` (the default) every outbound payload is
//! walked and any string value matching a sensitive pattern is replaced with a
//! redaction marker. Matching is on the *value*, so it catches secrets
//! regardless of which field they appear in.

use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};

/// Marker substituted for redacted values.
pub const REDACTED: &str = "[redacted]";
static REDACTION_COUNT: AtomicU64 = AtomicU64::new(0);

const SENSITIVE_KEY_TERMS: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "cookie",
    "set-cookie",
    "password",
    "passwd",
    "pwd",
    "api_key",
    "apikey",
    "x-api-key",
    "x-allstak-key",
    "x-auth-token",
    "x-access-token",
    "token",
    "bearer",
    "jwt",
    "secret",
    "credit_card",
    "card_number",
    "cvv",
    "ssn",
    "csrf",
];

const IDENTITY_KEY_EXEMPTIONS: &[&str] = &[
    "release",
    "environment",
    "sdkname",
    "sdkversion",
    "platform",
    "dist",
    "traceid",
    "spanid",
    "parentspanid",
    "requestid",
    "sessionid",
    "transactionid",
];

static EMAIL: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)[a-z0-9._%+\-]+@[a-z0-9.\-]+\.[a-z]{2,}").expect("email regex"));

// Credit-card-like: 13-16 digits, optionally grouped by spaces or dashes.
static CREDIT_CARD: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\b(?:\d[ -]*?){13,16}\b").expect("cc regex"));

// US SSN form: 3-2-4 digits separated by dashes or spaces.
static SSN: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\b\d{3}[- ]\d{2}[- ]\d{4}\b").expect("ssn regex"));

/// Whether a string contains a sensitive pattern.
pub fn looks_sensitive(s: &str) -> bool {
    EMAIL.is_match(s) || SSN.is_match(s) || is_credit_card(s)
}

fn is_credit_card(s: &str) -> bool {
    CREDIT_CARD.find_iter(s).any(|m| {
        let digits = m.as_str().chars().filter(|c| c.is_ascii_digit()).count();
        (13..=16).contains(&digits)
    })
}

/// Redact any sensitive substrings inside a single string.
pub fn scrub_string(s: &str) -> String {
    let mut out = SSN.replace_all(s, REDACTED).into_owned();
    out = EMAIL.replace_all(&out, REDACTED).into_owned();
    // Replace CC matches that carry 13-16 digits.
    out = CREDIT_CARD
        .replace_all(&out, |caps: &regex::Captures| {
            let m = caps.get(0).map(|m| m.as_str()).unwrap_or("");
            let digits = m.chars().filter(|c| c.is_ascii_digit()).count();
            if (13..=16).contains(&digits) {
                REDACTED.to_string()
            } else {
                m.to_string()
            }
        })
        .into_owned();
    if out != s {
        REDACTION_COUNT.fetch_add(1, Ordering::Relaxed);
    }
    out
}

/// Recursively scrub a JSON value in place.
pub fn scrub_value(value: &mut Value) {
    match value {
        Value::String(s) if looks_sensitive(s) => {
            *s = scrub_string(s);
        }
        Value::Array(arr) => {
            for v in arr.iter_mut() {
                scrub_value(v);
            }
        }
        Value::Object(map) => {
            for (k, v) in map.iter_mut() {
                if is_sensitive_key(k) {
                    REDACTION_COUNT.fetch_add(1, Ordering::Relaxed);
                    *v = Value::String(REDACTED.to_string());
                } else if is_identity_key(k) {
                    // SDK identity/correlation fields are not user metadata.
                    // Timestamped release names can look like credit-card-ish
                    // digit runs; preserving them keeps dashboard release and
                    // trace filtering usable without weakening secret-key
                    // redaction above.
                    continue;
                } else {
                    scrub_value(v);
                }
            }
        }
        _ => {}
    }
}

/// Process-wide sanitizer redaction count. Counter only; no payload data.
pub fn redaction_count() -> u64 {
    REDACTION_COUNT.load(Ordering::Relaxed)
}

fn is_sensitive_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    SENSITIVE_KEY_TERMS.iter().any(|term| lower.contains(term))
}

fn is_identity_key(key: &str) -> bool {
    let compact = key
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    IDENTITY_KEY_EXEMPTIONS.iter().any(|term| compact == *term)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_email() {
        assert_eq!(
            scrub_string("ping user@example.com now"),
            "ping [redacted] now"
        );
    }

    #[test]
    fn redacts_ssn() {
        assert_eq!(scrub_string("ssn 123-45-6789"), "ssn [redacted]");
    }

    #[test]
    fn redacts_credit_card() {
        assert_eq!(scrub_string("card 4111 1111 1111 1111"), "card [redacted]");
        assert_eq!(scrub_string("card 4111-1111-1111-1111"), "card [redacted]");
    }

    #[test]
    fn leaves_clean_text() {
        assert_eq!(scrub_string("nothing here"), "nothing here");
        assert!(!looks_sensitive("order 42 shipped"));
    }

    #[test]
    fn scrubs_nested_json() {
        let mut v = serde_json::json!({
            "a": "user@example.com",
            "b": ["123-45-6789", "ok"],
            "c": { "card": "4111111111111111" }
        });
        scrub_value(&mut v);
        assert_eq!(v["a"], serde_json::json!("[redacted]"));
        assert_eq!(v["b"][0], serde_json::json!("[redacted]"));
        assert_eq!(v["b"][1], serde_json::json!("ok"));
        assert_eq!(v["c"]["card"], serde_json::json!("[redacted]"));
    }

    #[test]
    fn scrubs_sensitive_keys() {
        let mut v = serde_json::json!({
            "Authorization": "Bearer abc",
            "nested": { "apiKey": "key-123" },
            "safe": "ok"
        });
        scrub_value(&mut v);
        assert_eq!(v["Authorization"], serde_json::json!("[redacted]"));
        assert_eq!(v["nested"]["apiKey"], serde_json::json!("[redacted]"));
        assert_eq!(v["safe"], serde_json::json!("ok"));
    }

    #[test]
    fn preserves_identity_fields_that_look_like_numeric_secrets() {
        let mut v = serde_json::json!({
            "release": "20260605054443-rust1",
            "environment": "dev-sdk-audit",
            "sdkName": "allstak-rust",
            "sdkVersion": "0.2.2",
            "traceId": "12345678901234567890123456789012",
            "message": "card 4111 1111 1111 1111",
            "metadata": {
                "password": "20260605054443-rust1"
            }
        });
        scrub_value(&mut v);
        assert_eq!(v["release"], serde_json::json!("20260605054443-rust1"));
        assert_eq!(v["environment"], serde_json::json!("dev-sdk-audit"));
        assert_eq!(v["sdkName"], serde_json::json!("allstak-rust"));
        assert_eq!(v["sdkVersion"], serde_json::json!("0.2.2"));
        assert_eq!(
            v["traceId"],
            serde_json::json!("12345678901234567890123456789012")
        );
        assert_eq!(v["message"], serde_json::json!("card [redacted]"));
        assert_eq!(v["metadata"]["password"], serde_json::json!("[redacted]"));
    }
}
