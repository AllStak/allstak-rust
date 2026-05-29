//! Wire-protocol types for the AllStak ingest API.
//!
//! Every struct here serializes with serde to the exact camelCase field names
//! required by the AllStak ingest contract. Optional fields are skipped when
//! `None` so payloads stay compact and match the documented shape.
//!
//! Field-level docs are intentionally omitted: each field maps one-to-one to
//! the named field in the AllStak ingest wire contract, so the field name *is*
//! the documentation. See the crate README for the full contract.
#![allow(missing_docs)]

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Severity level for events, logs and breadcrumbs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Debug,
    #[default]
    Info,
    Warning,
    Error,
    Fatal,
}

impl Level {
    /// Lower-case wire string for this level.
    pub fn as_str(&self) -> &'static str {
        match self {
            Level::Debug => "debug",
            Level::Info => "info",
            Level::Warning => "warning",
            Level::Error => "error",
            Level::Fatal => "fatal",
        }
    }

    /// Whether an event at this level should mark the active session errored.
    pub fn is_error(&self) -> bool {
        matches!(self, Level::Error | Level::Fatal)
    }
}

/// Identified user attached to an event or scope.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
}

/// A single resolved stack frame.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Frame {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub abs_path: Option<String>,
    #[serde(rename = "function", skip_serializing_if = "Option::is_none")]
    pub function: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lineno: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub colno: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_app: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debug_id: Option<String>,
}

/// HTTP request context for an error event.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestContext {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_code: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
}

/// A trail-of-events breadcrumb.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Breadcrumb {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub ty: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<Level>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl Default for Breadcrumb {
    fn default() -> Self {
        Breadcrumb {
            timestamp: Some(crate::util::now_iso8601()),
            ty: Some("default".to_string()),
            category: None,
            message: None,
            level: Some(Level::Info),
            data: None,
        }
    }
}

impl Breadcrumb {
    /// Construct a breadcrumb with a message and `info` level.
    pub fn new(message: impl Into<String>) -> Self {
        Breadcrumb {
            message: Some(message.into()),
            ..Breadcrumb::default()
        }
    }
}

/// Error event posted to `/ingest/v1/errors`.
///
/// Field names match the ingest contract exactly.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorEvent {
    /// Local-only event identifier. Not serialized to the wire.
    #[serde(skip)]
    pub event_id: Uuid,

    pub exception_class: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack_trace: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<User>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_context: Option<RequestContext>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub breadcrumbs: Option<Vec<Breadcrumb>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sdk_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sdk_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dist: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frames: Option<Vec<Frame>>,
}

impl ErrorEvent {
    /// Create a bare event with the given exception class and message.
    pub fn new(exception_class: impl Into<String>, message: impl Into<String>) -> Self {
        ErrorEvent {
            event_id: Uuid::new_v4(),
            exception_class: exception_class.into(),
            message: message.into(),
            stack_trace: None,
            level: None,
            environment: None,
            release: None,
            session_id: None,
            trace_id: None,
            user: None,
            metadata: None,
            request_context: None,
            breadcrumbs: None,
            sdk_name: None,
            sdk_version: None,
            platform: None,
            dist: None,
            frames: None,
        }
    }
}

/// A single HTTP request record for `/ingest/v1/http-requests`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpRequestRecord {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    pub direction: String,
    pub method: String,
    pub host: String,
    pub path: String,
    pub status_code: u16,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_fingerprint: Option<String>,
    pub timestamp: String,
}

/// Batch wrapper for HTTP request records.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpRequestBatch {
    pub requests: Vec<HttpRequestRecord>,
}

/// A single performance span for `/ingest/v1/spans`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpanRecord {
    pub trace_id: String,
    pub span_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_span_id: Option<String>,
    pub operation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    pub duration_ms: u64,
    pub start_time_millis: u64,
    pub end_time_millis: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// Batch wrapper for spans.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpanBatch {
    pub spans: Vec<SpanRecord>,
}

/// Structured log record for `/ingest/v1/logs`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogRecord {
    pub level: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Database query record for `/ingest/v1/db`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DbQueryRecord {
    pub normalized_query: String,
    pub query_hash: String,
    pub query_type: String,
    pub duration_ms: u64,
    pub timestamp_millis: u64,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub database_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub database_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span_id: Option<String>,
}

/// Batch wrapper for DB queries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbQueryBatch {
    pub queries: Vec<DbQueryRecord>,
}

/// Lifecycle status reported when ending a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionStatus {
    Ok,
    Errored,
    Crashed,
    Abnormal,
    Exited,
}

impl SessionStatus {
    /// Lower-case wire string for this status.
    pub fn as_str(&self) -> &'static str {
        match self {
            SessionStatus::Ok => "ok",
            SessionStatus::Errored => "errored",
            SessionStatus::Crashed => "crashed",
            SessionStatus::Abnormal => "abnormal",
            SessionStatus::Exited => "exited",
        }
    }
}

/// Payload for `/ingest/v1/sessions/start`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStart {
    pub session_id: String,
    pub release: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sdk_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sdk_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
}

/// Payload for `/ingest/v1/sessions/end`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEnd {
    pub session_id: String,
    pub release: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sdk_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sdk_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    pub duration_ms: u64,
    pub status: String,
}

/// Payload for `/ingest/v1/heartbeat`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Heartbeat {
    pub slug: String,
    pub status: String,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Payload for `/ingest/v1/releases`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseRegistration {
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
}
