//! Small helpers: timestamps, ids, and the SDK identity constants.

use std::time::{SystemTime, UNIX_EPOCH};

use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use uuid::Uuid;

/// Wire name reported in `sdkName` fields and the User-Agent.
pub const SDK_NAME: &str = "rust";

/// Crate version, taken from Cargo at build time.
pub const SDK_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Platform string reported in events.
pub const PLATFORM: &str = "rust";

/// `User-Agent` header value, e.g. `allstak-rust/0.1.0`.
pub fn user_agent() -> String {
    format!("allstak-rust/{SDK_VERSION}")
}

/// Current time as an RFC-3339 / ISO-8601 string in UTC.
pub fn now_iso8601() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

/// Current Unix time in milliseconds.
pub fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A 32-char lower-hex trace id (W3C-compatible width).
pub fn new_trace_id() -> String {
    Uuid::new_v4().simple().to_string()
}

/// A 16-char lower-hex span id (W3C-compatible width).
pub fn new_span_id() -> String {
    let full = Uuid::new_v4().simple().to_string();
    full[..16].to_string()
}
