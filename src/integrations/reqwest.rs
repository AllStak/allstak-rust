//! Outbound HTTP auto-instrumentation (`reqwest-middleware` feature).
//!
//! [`AllstakHttpMiddleware`] is a [`reqwest_middleware::Middleware`] that, for
//! every outbound request, with zero per-call code:
//!
//! - opens an `http.client` child span under the active scope's trace/span,
//! - injects the active trace context (`traceparent` + `X-AllStak-*`) into the
//!   outbound request headers so the downstream service continues the trace,
//! - records an outbound [`HttpRequestRecord`] to `/ingest/v1/http-requests`
//!   with the status, duration, host, path and trace ids.
//!
//! ## Zero-config usage
//!
//! ```no_run
//! # #[cfg(feature = "reqwest-middleware")]
//! # fn demo() -> reqwest_middleware::ClientWithMiddleware {
//! // After `allstak::init(...)`, wrap your reqwest client once:
//! let client = allstak::instrumented_http_client();
//! // Every request through `client` is now traced + recorded automatically.
//! client
//! # }
//! ```
//!
//! Or attach the middleware to an existing builder:
//!
//! ```no_run
//! # #[cfg(feature = "reqwest-middleware")]
//! # fn demo() -> reqwest_middleware::ClientWithMiddleware {
//! use reqwest_middleware::ClientBuilder;
//! ClientBuilder::new(reqwest::Client::new())
//!     .with(allstak::integrations::reqwest::AllstakHttpMiddleware::new())
//!     .build()
//! # }
//! ```

use std::time::Instant;

use http::Extensions;
use reqwest::{Request, Response};
use reqwest_middleware::{Error, Middleware, Next, Result};

use crate::hub::Hub;
use crate::performance::Span;
use crate::propagation;
use crate::protocol::HttpRequestRecord;
use crate::util;

/// reqwest middleware that traces and records outbound HTTP requests.
///
/// Behaviour is individually toggleable: span emission, header injection and
/// request recording can each be turned off, but all are on by default.
#[derive(Clone, Debug)]
pub struct AllstakHttpMiddleware {
    start_span: bool,
    inject_headers: bool,
    record_request: bool,
    operation: &'static str,
}

impl Default for AllstakHttpMiddleware {
    fn default() -> Self {
        AllstakHttpMiddleware {
            start_span: true,
            inject_headers: true,
            record_request: true,
            operation: "http.client",
        }
    }
}

impl AllstakHttpMiddleware {
    /// New middleware with span, header injection and request recording all on.
    pub fn new() -> Self {
        AllstakHttpMiddleware::default()
    }

    /// Toggle opening an `http.client` child span per request.
    pub fn enable_span(mut self, enable: bool) -> Self {
        self.start_span = enable;
        self
    }

    /// Toggle injecting `traceparent` / `X-AllStak-*` headers downstream.
    pub fn enable_header_injection(mut self, enable: bool) -> Self {
        self.inject_headers = enable;
        self
    }

    /// Toggle recording an outbound [`HttpRequestRecord`].
    pub fn enable_request_record(mut self, enable: bool) -> Self {
        self.record_request = enable;
        self
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl Middleware for AllstakHttpMiddleware {
    async fn handle(
        &self,
        mut req: Request,
        extensions: &mut Extensions,
        next: Next<'_>,
    ) -> Result<Response> {
        let hub = Hub::current();

        // Snapshot the active trace context (set by inbound middleware or the
        // active transaction). When there is no active trace, mint one so the
        // outbound call is still traceable end-to-end.
        let mut ctx = hub.current_trace_context();
        if ctx.trace_id.is_none() {
            ctx.trace_id = Some(util::new_trace_id());
        }

        let method = req.method().to_string();
        let url = req.url().clone();
        let host = url.host_str().unwrap_or("").to_string();
        let path = url.path().to_string();

        // Open an http.client child span under the active span.
        let mut span = if self.start_span {
            Some(Span::continued(
                self.operation,
                format!("{method} {host}{path}"),
                ctx.trace_id.clone(),
                ctx.parent_span_id.clone(),
            ))
        } else {
            None
        };
        let span_id = span.as_ref().map(|s| s.span_id().to_string());

        // Inject the active trace context into the outbound headers. The span
        // we just opened becomes the downstream parent.
        if self.inject_headers {
            let headers = req.headers_mut();
            propagation::inject(&ctx, span_id.as_deref(), |name, value| {
                if let (Ok(hn), Ok(hv)) = (
                    http::HeaderName::from_bytes(name.as_bytes()),
                    http::HeaderValue::from_str(value),
                ) {
                    headers.insert(hn, hv);
                }
            });
        }

        let started = Instant::now();
        let result = next.run(req, extensions).await;
        let duration_ms = started.elapsed().as_millis() as u64;

        // Resolve the outcome status: HTTP status on success, 0 on a transport
        // error (no response was produced).
        let status_code = match &result {
            Ok(resp) => resp.status().as_u16(),
            Err(Error::Reqwest(e)) => e.status().map(|s| s.as_u16()).unwrap_or(0),
            Err(_) => 0,
        };

        if let Some(span) = span.as_mut() {
            if status_code >= 500 || status_code == 0 {
                span.set_status("internal_error");
            } else {
                span.set_status("ok");
            }
            span.set_tag("http.method", method.clone());
            span.set_tag("http.host", host.clone());
            span.set_tag("http.status_code", status_code.to_string());
        }
        // Finish the span (records to /ingest/v1/spans).
        if let Some(span) = span.take() {
            span.finish();
        }

        if self.record_request {
            let record = HttpRequestRecord {
                trace_id: ctx.trace_id.clone(),
                request_id: ctx.request_id.clone(),
                direction: "outbound".to_string(),
                method,
                host,
                path,
                status_code,
                duration_ms,
                request_size: None,
                response_size: None,
                user_id: None,
                error_fingerprint: None,
                timestamp: util::now_iso8601(),
            };
            if let Some(client) = hub.client() {
                client.capture_http_request(record);
            }
        }

        result
    }
}

/// Build a fully-instrumented [`reqwest_middleware::ClientWithMiddleware`] from
/// a fresh default [`reqwest::Client`].
pub fn instrumented_client() -> reqwest_middleware::ClientWithMiddleware {
    instrumented_client_from(reqwest::Client::new())
}

/// Wrap an existing [`reqwest::Client`] with the AllStak HTTP middleware.
pub fn instrumented_client_from(
    client: reqwest::Client,
) -> reqwest_middleware::ClientWithMiddleware {
    reqwest_middleware::ClientBuilder::new(client)
        .with(AllstakHttpMiddleware::new())
        .build()
}
