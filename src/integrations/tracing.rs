//! `tracing` integration.
//!
//! [`layer`] returns a [`tracing_subscriber::Layer`] that turns `tracing`
//! events into AllStak logs, breadcrumbs or error events (per [`EventFilter`]),
//! and instrumented spans into AllStak performance spans. Event fields become
//! structured data; fields prefixed `tags.` become span/event tags.

use std::collections::BTreeMap;

use serde_json::{Map, Value};
use tracing_core::field::{Field, Visit};
use tracing_core::{Event as TracingEvent, Level as TLevel, Metadata, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

use crate::hub::Hub;
use crate::performance::Span as AllstakSpan;
use crate::protocol::{Breadcrumb, ErrorEvent, Level, LogRecord};
use crate::util;

/// What a `tracing` event is converted into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventFilter {
    /// Drop the event entirely.
    Ignore,
    /// Attach as a breadcrumb on the active scope.
    Breadcrumb,
    /// Capture as an AllStak error event.
    Event,
    /// Capture as an error event with a synthesized exception.
    Exception,
    /// Emit as a structured log record.
    Log,
}

/// Map a `tracing` level to the default [`EventFilter`].
fn default_event_filter(meta: &Metadata<'_>) -> EventFilter {
    match *meta.level() {
        TLevel::ERROR => EventFilter::Exception,
        TLevel::WARN | TLevel::INFO => EventFilter::Breadcrumb,
        _ => EventFilter::Ignore,
    }
}

fn to_level(level: &TLevel) -> Level {
    match *level {
        TLevel::ERROR => Level::Error,
        TLevel::WARN => Level::Warning,
        TLevel::INFO => Level::Info,
        TLevel::DEBUG => Level::Debug,
        TLevel::TRACE => Level::Debug,
    }
}

type FilterFn = Box<dyn Fn(&Metadata<'_>) -> EventFilter + Send + Sync>;
type SpanFilterFn = Box<dyn Fn(&Metadata<'_>) -> bool + Send + Sync>;

/// The `tracing` layer.
pub struct AllstakLayer {
    event_filter: FilterFn,
    span_filter: SpanFilterFn,
}

impl Default for AllstakLayer {
    fn default() -> Self {
        AllstakLayer {
            event_filter: Box::new(default_event_filter),
            span_filter: Box::new(|_| true),
        }
    }
}

impl AllstakLayer {
    /// Override how each event maps to an [`EventFilter`].
    pub fn event_filter<F>(mut self, f: F) -> Self
    where
        F: Fn(&Metadata<'_>) -> EventFilter + Send + Sync + 'static,
    {
        self.event_filter = Box::new(f);
        self
    }

    /// Override which spans are recorded.
    pub fn span_filter<F>(mut self, f: F) -> Self
    where
        F: Fn(&Metadata<'_>) -> bool + Send + Sync + 'static,
    {
        self.span_filter = Box::new(f);
        self
    }
}

/// Build a fresh [`AllstakLayer`].
pub fn layer() -> AllstakLayer {
    AllstakLayer::default()
}

/// Collects a `tracing` event/span's fields into message + structured data,
/// splitting out `tags.`-prefixed fields.
#[derive(Default)]
struct FieldVisitor {
    message: Option<String>,
    data: Map<String, Value>,
    tags: BTreeMap<String, String>,
}

impl FieldVisitor {
    fn record(&mut self, name: &str, value: Value) {
        if name == "message" {
            if let Value::String(s) = &value {
                self.message = Some(s.clone());
                return;
            }
        }
        if let Some(tag) = name.strip_prefix("tags.") {
            let s = match &value {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            self.tags.insert(tag.to_string(), s);
        } else {
            self.data.insert(name.to_string(), value);
        }
    }
}

impl Visit for FieldVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.record(field.name(), Value::String(format!("{value:?}")));
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        self.record(field.name(), Value::String(value.to_string()));
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.record(field.name(), Value::from(value));
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.record(field.name(), Value::from(value));
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.record(field.name(), Value::Bool(value));
    }
}

impl<S> Layer<S> for AllstakLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &TracingEvent<'_>, _ctx: Context<'_, S>) {
        let filter = (self.event_filter)(event.metadata());
        if filter == EventFilter::Ignore {
            return;
        }

        let mut visitor = FieldVisitor::default();
        event.record(&mut visitor);

        let level = to_level(event.metadata().level());
        let message = visitor
            .message
            .clone()
            .unwrap_or_else(|| event.metadata().target().to_string());
        let metadata = if visitor.data.is_empty() {
            None
        } else {
            Some(Value::Object(visitor.data.clone()))
        };

        let hub = Hub::current();
        match filter {
            EventFilter::Ignore => {}
            EventFilter::Breadcrumb => {
                hub.add_breadcrumb(Breadcrumb {
                    timestamp: Some(util::now_iso8601()),
                    ty: Some("log".to_string()),
                    category: Some(event.metadata().target().to_string()),
                    message: Some(message),
                    level: Some(level),
                    data: metadata,
                });
            }
            EventFilter::Log => {
                if let Some(client) = hub.client() {
                    let opts = client.options();
                    client.capture_log(LogRecord {
                        level: level.as_str().to_string(),
                        message,
                        service: opts.server_name.clone(),
                        environment: Some(opts.resolved_environment()),
                        trace_id: None,
                        span_id: None,
                        request_id: None,
                        user_id: None,
                        error_id: None,
                        metadata,
                    });
                }
            }
            EventFilter::Event | EventFilter::Exception => {
                let mut ev = ErrorEvent::new(event.metadata().target().to_string(), message);
                ev.level = Some(level.as_str().to_string());
                ev.metadata = metadata;
                hub.capture_event(ev);
            }
        }
    }

    fn on_new_span(
        &self,
        attrs: &tracing_core::span::Attributes<'_>,
        id: &tracing_core::span::Id,
        ctx: Context<'_, S>,
    ) {
        if !(self.span_filter)(attrs.metadata()) {
            return;
        }
        let Some(span_ref) = ctx.span(id) else {
            return;
        };

        let mut visitor = FieldVisitor::default();
        attrs.record(&mut visitor);

        // Special fields `as.name` / `as.op` override the span name/operation.
        let op = visitor
            .data
            .get("as.op")
            .and_then(|v| v.as_str())
            .unwrap_or("default")
            .to_string();
        let name = visitor
            .data
            .get("as.name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| attrs.metadata().name().to_string());

        let mut span = AllstakSpan::transaction(op, name);
        for (k, v) in &visitor.tags {
            span.set_tag(k.clone(), v.clone());
        }
        if !visitor.data.is_empty() {
            span.set_data(Value::Object(visitor.data.clone()));
        }

        // Stash the span so it finishes (and is sent) when the tracing span
        // closes.
        span_ref.extensions_mut().insert(SpanHolder(Some(span)));
    }

    fn on_close(&self, id: tracing_core::span::Id, ctx: Context<'_, S>) {
        if let Some(span_ref) = ctx.span(&id) {
            if let Some(holder) = span_ref.extensions_mut().get_mut::<SpanHolder>() {
                if let Some(span) = holder.0.take() {
                    span.finish();
                }
            }
        }
    }
}

struct SpanHolder(Option<AllstakSpan>);
