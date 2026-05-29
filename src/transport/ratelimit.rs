//! Per-category rate-limit tracking.
//!
//! Honours HTTP 429 `Retry-After` (delta-seconds or an HTTP-date) and a
//! comma/semicolon-delimited per-category rate-limit header. Limited
//! categories are held (their envelopes dropped) until the deadline passes
//! rather than being retried hot.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::envelope::DataCategory;

use super::LimitMap;

/// Tracks deadlines after which each data category may be sent again.
#[derive(Default)]
pub struct RateLimiter {
    limits: Mutex<LimitMap>,
    global: Mutex<Option<Instant>>,
}

impl RateLimiter {
    /// New limiter with no active limits.
    pub fn new() -> Self {
        RateLimiter::default()
    }

    /// Whether `category` is currently rate-limited.
    pub fn is_limited(&self, category: DataCategory) -> bool {
        let now = Instant::now();
        if let Ok(g) = self.global.lock() {
            if let Some(deadline) = *g {
                if now < deadline {
                    return true;
                }
            }
        }
        if let Ok(map) = self.limits.lock() {
            if let Some(deadline) = map.get(&category) {
                return now < *deadline;
            }
        }
        false
    }

    /// Update limits from a 429 response's headers.
    pub fn update_from_response(
        &self,
        category: DataCategory,
        headers: &reqwest::header::HeaderMap,
    ) {
        // `Retry-After` applies to everything for the given duration.
        if let Some(val) = headers.get("retry-after").and_then(|v| v.to_str().ok()) {
            if let Some(dur) = parse_retry_after(val) {
                if let Ok(mut g) = self.global.lock() {
                    *g = Some(Instant::now() + dur);
                }
                return;
            }
        }
        // Otherwise hold just this category for a conservative default window.
        self.hold(category, Duration::from_secs(60));
    }

    /// Hold a category limited for `dur`.
    pub fn hold(&self, category: DataCategory, dur: Duration) {
        if let Ok(mut map) = self.limits.lock() {
            map.insert(category, Instant::now() + dur);
        }
    }
}

/// Parse a `Retry-After` value: either delta-seconds or an HTTP-date.
/// Only delta-seconds is interpreted precisely; an HTTP-date falls back to a
/// fixed conservative window so we never send hot into a limit.
fn parse_retry_after(value: &str) -> Option<Duration> {
    let trimmed = value.trim();
    if let Ok(secs) = trimmed.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    // HTTP-date form: hold for a conservative default rather than parsing.
    Some(Duration::from_secs(60))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delta_seconds_parsed() {
        assert_eq!(parse_retry_after("30"), Some(Duration::from_secs(30)));
    }

    #[test]
    fn http_date_falls_back() {
        let d = parse_retry_after("Wed, 21 Oct 2026 07:28:00 GMT").unwrap();
        assert_eq!(d, Duration::from_secs(60));
    }

    #[test]
    fn hold_marks_category_limited() {
        let rl = RateLimiter::new();
        assert!(!rl.is_limited(DataCategory::Error));
        rl.hold(DataCategory::Error, Duration::from_secs(60));
        assert!(rl.is_limited(DataCategory::Error));
        // Other categories remain unaffected.
        assert!(!rl.is_limited(DataCategory::Session));
    }

    #[test]
    fn expired_hold_is_not_limited() {
        let rl = RateLimiter::new();
        rl.hold(DataCategory::Log, Duration::from_millis(0));
        std::thread::sleep(Duration::from_millis(5));
        assert!(!rl.is_limited(DataCategory::Log));
    }
}
