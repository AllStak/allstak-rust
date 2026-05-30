//! Database query helpers: statement normalization, fingerprinting, and a
//! convenience for recording a [`DbQueryRecord`] tied to the active span.
//!
//! These helpers back the feature-gated driver integrations (e.g. the `sqlx`
//! tracing layer) and are re-exported at the crate root so applications can
//! record DB queries by hand when a driver hook is not available.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::hub::Hub;
use crate::protocol::DbQueryRecord;
use crate::util;

/// Normalize a SQL statement for grouping: collapse whitespace and replace
/// literal values (numbers, quoted strings) with `?` placeholders so queries
/// that differ only by their bound values share a fingerprint. PII in literals
/// is dropped as a side effect.
pub fn normalize_query(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut chars = sql.chars().peekable();
    let mut last_was_space = false;

    while let Some(c) = chars.next() {
        match c {
            // Single-quoted string literal -> ?
            '\'' => {
                while let Some(&n) = chars.peek() {
                    chars.next();
                    if n == '\'' {
                        // Handle escaped '' inside the literal.
                        if chars.peek() == Some(&'\'') {
                            chars.next();
                            continue;
                        }
                        break;
                    }
                }
                out.push('?');
                last_was_space = false;
            }
            // Numeric literal -> ? (only when it starts a token).
            c if c.is_ascii_digit()
                && out
                    .chars()
                    .last()
                    .map(|p| !p.is_alphanumeric() && p != '_')
                    .unwrap_or(true) =>
            {
                while let Some(&n) = chars.peek() {
                    if n.is_ascii_digit() || n == '.' {
                        chars.next();
                    } else {
                        break;
                    }
                }
                out.push('?');
                last_was_space = false;
            }
            // Collapse runs of whitespace into a single space.
            c if c.is_whitespace() => {
                if !last_was_space && !out.is_empty() {
                    out.push(' ');
                    last_was_space = true;
                }
            }
            other => {
                out.push(other);
                last_was_space = false;
            }
        }
    }

    out.trim().to_string()
}

/// Stable lower-hex fingerprint for a normalized query string.
pub fn query_hash(normalized: &str) -> String {
    let mut hasher = DefaultHasher::new();
    normalized.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Classify a query by its leading keyword (`SELECT`, `INSERT`, ... or
/// `OTHER`).
pub fn query_type(sql: &str) -> String {
    let kw = sql
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    match kw.as_str() {
        "SELECT" | "INSERT" | "UPDATE" | "DELETE" | "BEGIN" | "COMMIT" | "ROLLBACK" | "CREATE"
        | "DROP" | "ALTER" | "WITH" | "UPSERT" | "MERGE" | "CALL" | "EXEC" | "EXECUTE" | "SET"
        | "SAVEPOINT" | "RELEASE" => kw,
        _ => "OTHER".to_string(),
    }
}

/// Build a [`DbQueryRecord`] from a raw statement and observed metrics, tying
/// it to the active scope's trace/span. The statement is normalized (no
/// literals) before being stored.
pub fn build_record(
    sql: &str,
    duration_ms: u64,
    error_message: Option<String>,
    database_type: Option<String>,
) -> DbQueryRecord {
    let normalized = normalize_query(sql);
    let hash = query_hash(&normalized);
    let qtype = query_type(sql);

    let hub = Hub::current();
    let ctx = hub.current_trace_context();
    let (service, environment) = hub
        .client()
        .map(|c| {
            let o = c.options();
            (o.server_name.clone(), Some(o.resolved_environment()))
        })
        .unwrap_or((None, None));

    let status = if error_message.is_some() {
        "error".to_string()
    } else {
        "ok".to_string()
    };

    DbQueryRecord {
        normalized_query: normalized,
        query_hash: hash,
        query_type: qtype,
        duration_ms,
        timestamp_millis: util::now_millis(),
        status,
        error_message,
        database_name: None,
        database_type,
        service,
        environment,
        trace_id: ctx.trace_id,
        span_id: ctx.parent_span_id,
    }
}

/// Record a single DB query against the current hub's client. Convenience for
/// driver integrations and manual instrumentation: builds a normalized record
/// tied to the active span and sends it to `/ingest/v1/db`.
pub fn capture_query(
    sql: &str,
    duration_ms: u64,
    error_message: Option<String>,
    database_type: Option<String>,
) {
    let record = build_record(sql, duration_ms, error_message, database_type);
    if let Some(client) = Hub::current().client() {
        client.capture_db_queries(vec![record]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_literals_and_whitespace() {
        let n = normalize_query("SELECT *  FROM users\n WHERE id = 42 AND name = 'a''b'");
        assert_eq!(n, "SELECT * FROM users WHERE id = ? AND name = ?");
    }

    #[test]
    fn hash_is_stable_and_groups_by_shape() {
        let a = query_hash(&normalize_query("SELECT * FROM t WHERE id = 1"));
        let b = query_hash(&normalize_query("SELECT * FROM t WHERE id = 2"));
        assert_eq!(a, b, "queries differing only by literal share a hash");
    }

    #[test]
    fn classifies_query_type() {
        assert_eq!(query_type("  select 1"), "SELECT");
        assert_eq!(query_type("INSERT INTO t VALUES (1)"), "INSERT");
        assert_eq!(query_type("VACUUM"), "OTHER");
    }
}
