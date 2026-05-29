//! Performance tracing: transactions and spans posted to `/ingest/v1/spans`.

use std::collections::BTreeMap;
use std::time::Instant;

use crate::hub::Hub;
use crate::protocol::SpanRecord;
use crate::util;

/// An in-flight span. On [`Span::finish`] (or drop) it is sent to the active
/// client as a span record.
pub struct Span {
    trace_id: String,
    span_id: String,
    parent_span_id: Option<String>,
    operation: String,
    description: Option<String>,
    status: Option<String>,
    tags: BTreeMap<String, String>,
    data: Option<serde_json::Value>,
    start_millis: u64,
    started: Instant,
    finished: bool,
}

impl Span {
    /// Start a root transaction with `operation` and a human `name`.
    pub fn transaction(operation: impl Into<String>, name: impl Into<String>) -> Span {
        Span {
            trace_id: util::new_trace_id(),
            span_id: util::new_span_id(),
            parent_span_id: None,
            operation: operation.into(),
            description: Some(name.into()),
            status: None,
            tags: BTreeMap::new(),
            data: None,
            start_millis: util::now_millis(),
            started: Instant::now(),
            finished: false,
        }
    }

    /// Start a child span sharing this span's trace id.
    pub fn start_child(
        &self,
        operation: impl Into<String>,
        description: impl Into<String>,
    ) -> Span {
        Span {
            trace_id: self.trace_id.clone(),
            span_id: util::new_span_id(),
            parent_span_id: Some(self.span_id.clone()),
            operation: operation.into(),
            description: Some(description.into()),
            status: None,
            tags: BTreeMap::new(),
            data: None,
            start_millis: util::now_millis(),
            started: Instant::now(),
            finished: false,
        }
    }

    /// Continue a span from inbound trace propagation headers.
    pub fn continued(
        operation: impl Into<String>,
        name: impl Into<String>,
        trace_id: Option<String>,
        parent_span_id: Option<String>,
    ) -> Span {
        Span {
            trace_id: trace_id.unwrap_or_else(util::new_trace_id),
            span_id: util::new_span_id(),
            parent_span_id,
            operation: operation.into(),
            description: Some(name.into()),
            status: None,
            tags: BTreeMap::new(),
            data: None,
            start_millis: util::now_millis(),
            started: Instant::now(),
            finished: false,
        }
    }

    /// The trace id this span belongs to.
    pub fn trace_id(&self) -> &str {
        &self.trace_id
    }

    /// This span's id.
    pub fn span_id(&self) -> &str {
        &self.span_id
    }

    /// Set the span status (e.g. `ok`, `internal_error`).
    pub fn set_status(&mut self, status: impl Into<String>) {
        self.status = Some(status.into());
    }

    /// Set a string tag on the span.
    pub fn set_tag(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.tags.insert(key.into(), value.into());
    }

    /// Attach structured data to the span.
    pub fn set_data(&mut self, data: serde_json::Value) {
        self.data = Some(data);
    }

    /// Finish the span and send it to the active client.
    pub fn finish(mut self) {
        self.send();
    }

    fn send(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;

        let hub = Hub::current();
        let Some(client) = hub.client() else {
            return;
        };
        let opts = client.options();
        let end_millis = util::now_millis();

        let record = SpanRecord {
            trace_id: self.trace_id.clone(),
            span_id: self.span_id.clone(),
            parent_span_id: self.parent_span_id.clone(),
            operation: self.operation.clone(),
            description: self.description.clone(),
            status: self.status.clone(),
            duration_ms: self.started.elapsed().as_millis() as u64,
            start_time_millis: self.start_millis,
            end_time_millis: end_millis,
            service: opts.server_name.clone(),
            environment: Some(opts.resolved_environment()),
            tags: if self.tags.is_empty() {
                None
            } else {
                Some(self.tags.clone())
            },
            data: self.data.clone(),
        };
        client.capture_span(record);
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        self.send();
    }
}

/// Start a root transaction. Convenience for [`Span::transaction`].
pub fn start_transaction(operation: impl Into<String>, name: impl Into<String>) -> Span {
    Span::transaction(operation, name)
}

/// Start a standalone span. Convenience for [`Span::transaction`].
pub fn start_span(operation: impl Into<String>, description: impl Into<String>) -> Span {
    Span::transaction(operation, description)
}
