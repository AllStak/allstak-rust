//! Database auto-instrumentation (`sqlx` feature).
//!
//! sqlx emits one `tracing` event per executed statement on the `sqlx::query`
//! target, carrying the statement summary, full SQL, row counts and elapsed
//! time. [`AllstakSqlxLayer`] is a [`tracing_subscriber::Layer`] that turns
//! those events into [`DbQueryRecord`]s tied to the active scope's trace/span,
//! posted to `/ingest/v1/db` — with **zero per-query code**.
//!
//! Because it consumes sqlx's own telemetry, this layer links no driver crate:
//! it works for any sqlx backend (Postgres, MySQL, SQLite, …). The layer is
//! installed alongside the AllStak `tracing` layer.
//!
//! ## Zero-config usage
//!
//! ```no_run
//! # #[cfg(all(feature = "sqlx", feature = "tracing"))]
//! # fn demo() {
//! use tracing_subscriber::prelude::*;
//!
//! let _guard = allstak::init("<key>");
//! tracing_subscriber::registry()
//!     .with(allstak::integrations::tracing::layer())
//!     .with(allstak::integrations::sqlx::layer())
//!     .init();
//! // Every sqlx query now produces a DbQueryRecord automatically.
//! # }
//! ```
//!
//! sqlx only emits the event when its `sqlx::query` log level is enabled (it is
//! by default at `INFO`); this layer enables it via its own interest.

use std::time::Duration;

use tracing_core::field::{Field, Visit};
use tracing_core::{Event as TracingEvent, Metadata, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

use crate::db;
use crate::hub::Hub;

/// The sqlx target sqlx emits its per-query telemetry on.
const SQLX_TARGET: &str = "sqlx::query";

type DbTypeFn = Box<dyn Fn(&Metadata<'_>) -> Option<String> + Send + Sync>;

/// A `tracing` layer that records sqlx query telemetry as AllStak DB queries.
pub struct AllstakSqlxLayer {
    /// Resolve a `databaseType` label for an event (default: `None`).
    database_type: DbTypeFn,
    /// Skip statements faster than this (default: zero — capture all).
    min_duration: Duration,
}

impl Default for AllstakSqlxLayer {
    fn default() -> Self {
        AllstakSqlxLayer {
            database_type: Box::new(|_| None),
            min_duration: Duration::ZERO,
        }
    }
}

impl AllstakSqlxLayer {
    /// New layer capturing every sqlx statement.
    pub fn new() -> Self {
        AllstakSqlxLayer::default()
    }

    /// Set a fixed `databaseType` label attached to every record.
    pub fn database_type(mut self, ty: impl Into<String>) -> Self {
        let ty = ty.into();
        self.database_type = Box::new(move |_| Some(ty.clone()));
        self
    }

    /// Only record statements taking at least `min` (sampling out fast ones).
    pub fn min_duration(mut self, min: Duration) -> Self {
        self.min_duration = min;
        self
    }
}

/// Build a fresh [`AllstakSqlxLayer`].
pub fn layer() -> AllstakSqlxLayer {
    AllstakSqlxLayer::default()
}

/// Pulls the fields sqlx stamps on its query events.
#[derive(Default)]
struct SqlxVisitor {
    summary: Option<String>,
    statement: Option<String>,
    elapsed_secs: Option<f64>,
    rows_affected: Option<u64>,
    rows_returned: Option<u64>,
    message: Option<String>,
}

impl Visit for SqlxVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let s = format!("{value:?}");
        match field.name() {
            "summary" => self.summary = Some(strip_quotes(&s)),
            "db.statement" => self.statement = Some(strip_quotes(&s)),
            "message" => self.message = Some(strip_quotes(&s)),
            _ => {}
        }
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "summary" => self.summary = Some(value.to_string()),
            "db.statement" => self.statement = Some(value.to_string()),
            "message" => self.message = Some(value.to_string()),
            _ => {}
        }
    }
    fn record_f64(&mut self, field: &Field, value: f64) {
        if field.name() == "elapsed_secs" {
            self.elapsed_secs = Some(value);
        }
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        match field.name() {
            "rows_affected" => self.rows_affected = Some(value),
            "rows_returned" => self.rows_returned = Some(value),
            _ => {}
        }
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        match field.name() {
            "rows_affected" => self.rows_affected = Some(value as u64),
            "rows_returned" => self.rows_returned = Some(value as u64),
            _ => {}
        }
    }
}

/// `Debug`-formatted strings arrive wrapped in quotes; strip one layer.
fn strip_quotes(s: &str) -> String {
    let t = s.trim();
    if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') {
        t[1..t.len() - 1].replace("\\n", "\n").replace("\\\"", "\"")
    } else {
        t.to_string()
    }
}

impl<S> Layer<S> for AllstakSqlxLayer
where
    S: Subscriber,
{
    fn on_event(&self, event: &TracingEvent<'_>, _ctx: Context<'_, S>) {
        if event.metadata().target() != SQLX_TARGET {
            return;
        }
        // Only act when there is a live client to send to.
        let hub = Hub::current();
        if hub.client().is_none() {
            return;
        }

        let mut v = SqlxVisitor::default();
        event.record(&mut v);

        // The full statement, when present, is the most useful; fall back to
        // the summary sqlx always provides.
        let sql = match v.statement.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(s) => s.to_string(),
            None => match &v.summary {
                Some(s) => s.clone(),
                None => return,
            },
        };

        let duration = Duration::from_secs_f64(v.elapsed_secs.unwrap_or(0.0).max(0.0));
        if duration < self.min_duration {
            return;
        }
        let duration_ms = duration.as_millis() as u64;

        let database_type = (self.database_type)(event.metadata());

        let mut record = db::build_record(&sql, duration_ms, None, database_type);

        // sqlx logs a "slow statement" warning above its alert threshold; the
        // DbQueryRecord wire status carries that as `slow` (the query itself
        // still succeeded, so it is not `error`).
        if let Some(msg) = &v.message {
            if msg.to_ascii_lowercase().contains("slow") {
                record.status = "slow".to_string();
            }
        }
        // `rows_returned` / `rows_affected` are observed but the DbQueryRecord
        // wire shape carries no row-count field, so they are intentionally not
        // forwarded (kept here for forward-compatibility of the visitor).
        let _ = (v.rows_returned, v.rows_affected);

        if let Some(client) = hub.client() {
            client.capture_db_queries(vec![record]);
        }
    }
}
