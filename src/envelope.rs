//! Envelope: a typed unit of work the transport delivers to a single ingest
//! endpoint. Each variant carries an already-serialized JSON body plus its
//! data category (used for rate-limit accounting).

use serde_json::Value;

/// Logical data category, used by the rate limiter to bucket limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DataCategory {
    /// Error events.
    Error,
    /// Performance transactions / spans.
    Transaction,
    /// Release-health session updates.
    Session,
    /// Structured log records.
    Log,
    /// Inbound/outbound HTTP request records.
    HttpRequest,
    /// Database query records.
    Db,
    /// Cron / heartbeat check-ins.
    Heartbeat,
    /// Release registrations.
    Release,
}

impl DataCategory {
    /// Stable string key for this category.
    pub fn as_str(&self) -> &'static str {
        match self {
            DataCategory::Error => "error",
            DataCategory::Transaction => "transaction",
            DataCategory::Session => "session",
            DataCategory::Log => "log",
            DataCategory::HttpRequest => "http_request",
            DataCategory::Db => "db",
            DataCategory::Heartbeat => "heartbeat",
            DataCategory::Release => "release",
        }
    }
}

/// A ready-to-send ingest item: endpoint path, category and JSON body.
#[derive(Debug, Clone)]
pub struct Envelope {
    /// Path relative to the configured host, e.g. `/ingest/v1/errors`.
    pub path: &'static str,
    /// Rate-limit category for this item.
    pub category: DataCategory,
    /// The JSON body to POST.
    pub body: Value,
}

impl Envelope {
    /// Build an envelope from a serializable payload, panicking only on a
    /// programming error (a payload that cannot be represented as JSON).
    pub fn new<T: serde::Serialize>(
        path: &'static str,
        category: DataCategory,
        payload: &T,
    ) -> Self {
        let body = serde_json::to_value(payload).unwrap_or(Value::Null);
        Envelope {
            path,
            category,
            body,
        }
    }
}
