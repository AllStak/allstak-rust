//! The [`Client`] owns options, transport and integrations, and runs the
//! capture pipeline that turns events into delivered envelopes.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use uuid::Uuid;

use crate::diagnostics::Diagnostics;
use crate::envelope::{DataCategory, Envelope};
use crate::options::ClientOptions;
use crate::protocol::{
    Breadcrumb, DbQueryBatch, DbQueryRecord, ErrorEvent, Heartbeat, HttpRequestBatch,
    HttpRequestRecord, LogRecord, ReleaseRegistration, SessionEnd, SessionStart, SpanBatch,
    SpanRecord,
};
use crate::scope::Scope;
use crate::scrub;
use crate::transport::{DefaultTransportFactory, Transport, TransportFactory};
use crate::util;

/// A bound client: configuration + transport + integration pipeline.
pub struct Client {
    options: ClientOptions,
    transport: Option<Arc<dyn Transport>>,
    /// Simple deterministic-friendly counter feeding the sample-rate dice roll.
    sample_counter: AtomicU64,
    events_captured: AtomicU64,
    events_dropped: AtomicU64,
}

impl Client {
    /// Build a client from options, running each integration's `setup` and
    /// creating the transport (unless the api key is empty, which yields a
    /// disabled client with no transport).
    pub fn new(mut options: ClientOptions) -> Arc<Client> {
        // Let integrations register hooks before the transport is built.
        let integrations = options.integrations.clone();
        for integration in &integrations {
            integration.setup(&mut options);
        }

        let transport = if options.api_key.is_empty() {
            None
        } else {
            let factory: Arc<dyn TransportFactory> = options
                .transport
                .clone()
                .unwrap_or_else(|| Arc::new(DefaultTransportFactory));
            Some(factory.create_transport(&options))
        };

        Arc::new(Client {
            options,
            transport,
            sample_counter: AtomicU64::new(0),
            events_captured: AtomicU64::new(0),
            events_dropped: AtomicU64::new(0),
        })
    }

    /// Whether this client has a live transport.
    pub fn is_enabled(&self) -> bool {
        self.transport.is_some()
    }

    /// The resolved options.
    pub fn options(&self) -> &ClientOptions {
        &self.options
    }

    /// Wait up to `timeout` for the transport queue to drain.
    pub fn flush(&self, timeout: Duration) -> bool {
        match &self.transport {
            Some(t) => t.flush(timeout),
            None => true,
        }
    }

    /// Drain and stop the transport.
    pub fn close(&self, timeout: Duration) -> bool {
        match &self.transport {
            Some(t) => t.shutdown(timeout),
            None => true,
        }
    }

    /// Run the capture pipeline for an error event against `scope`.
    ///
    /// Returns the event id if the event was accepted (it may still be dropped
    /// by `before_send`, sampling or rate limits — the id reflects acceptance
    /// into the pipeline, not delivery).
    pub fn capture_event(&self, mut event: ErrorEvent, scope: &Scope) -> Uuid {
        self.events_captured.fetch_add(1, Ordering::Relaxed);
        let event_id = event.event_id;

        // 1. Apply identity defaults from options.
        self.apply_defaults(&mut event);

        // 2. Apply active scope.
        scope.apply_to_event(&mut event);

        // 3. Run integrations in order.
        for integration in &self.options.integrations {
            match integration.process_event(event, &self.options) {
                Some(e) => event = e,
                None => {
                    self.events_dropped.fetch_add(1, Ordering::Relaxed);
                    return event_id;
                } // dropped by an integration
            }
        }

        // 4. Sanitize before before_send, then run hook. The delivery path
        // scrubs again after the hook, so callbacks cannot reintroduce secrets.
        if !self.options.send_default_pii {
            event = self.sanitized_event_for_hook(event);
        }
        if let Some(hook) = &self.options.before_send {
            match hook(event) {
                Some(e) => event = e,
                None => {
                    self.events_dropped.fetch_add(1, Ordering::Relaxed);
                    return event_id;
                }
            }
        }

        // 5. sample_rate dice roll.
        if !self.sample() {
            self.events_dropped.fetch_add(1, Ordering::Relaxed);
            return event_id;
        }

        // 6. Serialize, scrub, deliver.
        self.deliver(&event, "/ingest/v1/errors", DataCategory::Error);
        event_id
    }

    fn sanitized_event_for_hook(&self, event: ErrorEvent) -> ErrorEvent {
        let original = event.clone();
        let mut value = match serde_json::to_value(event) {
            Ok(value) => value,
            Err(_) => return self.redacted_event(original),
        };
        scrub::scrub_value(&mut value);
        match serde_json::from_value(value) {
            Ok(event) => event,
            Err(_) => self.redacted_event(original),
        }
    }

    fn redacted_event(&self, mut event: ErrorEvent) -> ErrorEvent {
        event.message = scrub::REDACTED.to_string();
        event.metadata = Some(serde_json::json!({ "redacted": true }));
        event.breadcrumbs = None;
        event
    }

    /// Default identity fields from options onto an event.
    fn apply_defaults(&self, event: &mut ErrorEvent) {
        if event.environment.is_none() {
            event.environment = Some(self.options.resolved_environment());
        }
        if event.release.is_none() {
            event.release = self.options.release.clone();
        }
        if event.sdk_name.is_none() {
            event.sdk_name = Some(util::SDK_NAME.to_string());
        }
        if event.sdk_version.is_none() {
            event.sdk_version = Some(util::SDK_VERSION.to_string());
        }
        if event.platform.is_none() {
            event.platform = Some(util::PLATFORM.to_string());
        }
    }

    /// Apply the `before_breadcrumb` hook, returning the (possibly mutated)
    /// breadcrumb or `None` if it should be dropped.
    pub(crate) fn process_breadcrumb(&self, breadcrumb: Breadcrumb) -> Option<Breadcrumb> {
        let processed = match &self.options.before_breadcrumb {
            Some(hook) => hook(breadcrumb),
            None => Some(breadcrumb),
        };
        if processed.is_none() {
            self.events_dropped.fetch_add(1, Ordering::Relaxed);
        }
        processed
    }

    fn sample(&self) -> bool {
        let rate = self.options.sample_rate.clamp(0.0, 1.0);
        if rate >= 1.0 {
            return true;
        }
        if rate <= 0.0 {
            return false;
        }
        // Deterministic 1-in-N style sampler avoiding an rng dependency.
        let n = self.sample_counter.fetch_add(1, Ordering::Relaxed);
        let bucket = (n % 1000) as f32 / 1000.0;
        bucket < rate
    }

    /// Serialize a payload, scrub PII when configured, and enqueue it.
    fn deliver<T: serde::Serialize>(
        &self,
        payload: &T,
        path: &'static str,
        category: DataCategory,
    ) {
        let Some(transport) = &self.transport else {
            self.events_dropped.fetch_add(1, Ordering::Relaxed);
            return;
        };
        let mut env = Envelope::new(path, category, payload);
        if !self.options.send_default_pii {
            scrub::scrub_value(&mut env.body);
        }
        transport.send_envelope(env);
    }

    // --- Direct ingest helpers used by spans/logs/sessions/middleware ---

    /// Send a batch of performance spans.
    pub fn capture_spans(&self, spans: Vec<SpanRecord>) {
        if spans.is_empty() {
            return;
        }
        self.events_captured.fetch_add(1, Ordering::Relaxed);
        let batch = SpanBatch { spans };
        self.deliver(&batch, "/ingest/v1/spans", DataCategory::Transaction);
    }

    /// Send a single span.
    pub fn capture_span(&self, span: SpanRecord) {
        self.capture_spans(vec![span]);
    }

    /// Send a structured log record.
    pub fn capture_log(&self, log: LogRecord) {
        self.events_captured.fetch_add(1, Ordering::Relaxed);
        self.deliver(&log, "/ingest/v1/logs", DataCategory::Log);
    }

    /// Send a batch of HTTP request records.
    pub fn capture_http_requests(&self, requests: Vec<HttpRequestRecord>) {
        if requests.is_empty() {
            return;
        }
        self.events_captured.fetch_add(1, Ordering::Relaxed);
        let batch = HttpRequestBatch { requests };
        self.deliver(
            &batch,
            "/ingest/v1/http-requests",
            DataCategory::HttpRequest,
        );
    }

    /// Send a single HTTP request record.
    pub fn capture_http_request(&self, request: HttpRequestRecord) {
        self.capture_http_requests(vec![request]);
    }

    /// Send a batch of DB query records.
    pub fn capture_db_queries(&self, queries: Vec<DbQueryRecord>) {
        if queries.is_empty() {
            return;
        }
        self.events_captured.fetch_add(1, Ordering::Relaxed);
        let batch = DbQueryBatch { queries };
        self.deliver(&batch, "/ingest/v1/db", DataCategory::Db);
    }

    /// Register the start of a session.
    pub fn send_session_start(&self, start: &SessionStart) {
        self.events_captured.fetch_add(1, Ordering::Relaxed);
        self.deliver(start, "/ingest/v1/sessions/start", DataCategory::Session);
    }

    /// Register the end of a session.
    pub fn send_session_end(&self, end: &SessionEnd) {
        self.events_captured.fetch_add(1, Ordering::Relaxed);
        self.deliver(end, "/ingest/v1/sessions/end", DataCategory::Session);
    }

    /// Send a heartbeat / cron check-in.
    pub fn send_heartbeat(&self, hb: &Heartbeat) {
        self.events_captured.fetch_add(1, Ordering::Relaxed);
        self.deliver(hb, "/ingest/v1/heartbeat", DataCategory::Heartbeat);
    }

    /// Register a release (best-effort).
    pub fn send_release(&self, release: &ReleaseRegistration) {
        self.events_captured.fetch_add(1, Ordering::Relaxed);
        self.deliver(release, "/ingest/v1/releases", DataCategory::Release);
    }

    /// Counter-only diagnostics. Scope-derived active trace/span/breadcrumb
    /// values are filled by [`crate::Hub::get_diagnostics`].
    pub fn get_diagnostics(&self) -> Diagnostics {
        let tx = self
            .transport
            .as_ref()
            .map(|t| t.diagnostics())
            .unwrap_or_default();
        Diagnostics {
            events_captured: self
                .events_captured
                .load(Ordering::Relaxed)
                .max(tx.events_captured),
            events_sent: tx.events_sent,
            events_failed: tx.events_failed,
            events_dropped: self.events_dropped.load(Ordering::Relaxed) + tx.events_dropped,
            events_persisted: tx.events_persisted,
            events_replayed: tx.events_replayed,
            queue_size: tx.queue_size,
            retry_attempts: tx.retry_attempts,
            rate_limited_count: tx.rate_limited_count,
            compressed_payloads: tx.compressed_payloads,
            uncompressed_payloads: tx.uncompressed_payloads,
            compression_bytes_saved: tx.compression_bytes_saved,
            sanitizer_redaction_count: scrub::redaction_count(),
            active_trace_count: 0,
            active_span_count: 0,
            breadcrumb_count: 0,
            session_recovery_count: 0,
            disabled: self.transport.is_none() || tx.disabled,
        }
    }
}
