//! axum / tower integration.
//!
//! [`AllstakLayer`] is a tower [`Layer`] that, for each inbound request:
//! binds a fresh per-request [`Hub`] (cloned from the top of the stack so
//! breadcrumbs/errors don't bleed across requests), continues an incoming
//! distributed trace, opens a request span, records the request to
//! `/ingest/v1/http-requests` (using the matched route template as the path
//! when available), and marks the request span/session errored on a 5xx.

use std::sync::Arc;
use std::task::{Context as TaskContext, Poll};
use std::time::Instant;

use axum::extract::MatchedPath;
use http::{Request, Response};
use tower::{Layer, Service};

use crate::hub::Hub;
use crate::performance::Span;
use crate::propagation;
use crate::protocol::HttpRequestRecord;
use crate::util;

/// Tower layer that installs the AllStak request middleware.
#[derive(Clone, Default)]
pub struct AllstakLayer {
    start_transaction: bool,
}

impl AllstakLayer {
    /// New layer. By default it records the request and opens a span.
    pub fn new() -> Self {
        AllstakLayer {
            start_transaction: true,
        }
    }

    /// Toggle opening a performance transaction span per request.
    pub fn enable_transaction(mut self, enable: bool) -> Self {
        self.start_transaction = enable;
        self
    }
}

impl<S> Layer<S> for AllstakLayer {
    type Service = AllstakService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        AllstakService {
            inner,
            start_transaction: self.start_transaction,
        }
    }
}

/// The tower service produced by [`AllstakLayer`].
#[derive(Clone)]
pub struct AllstakService<S> {
    inner: S,
    start_transaction: bool,
}

impl<S, ReqBody, ResBody> Service<Request<ReqBody>> for AllstakService<S>
where
    S: Service<Request<ReqBody>, Response = Response<ResBody>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    ReqBody: Send + 'static,
    ResBody: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>,
    >;

    fn poll_ready(&mut self, cx: &mut TaskContext<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<ReqBody>) -> Self::Future {
        // Per-request hub cloned from the top of the current stack.
        let hub = Hub::new_from_top(&Hub::current());

        let method = req.method().to_string();
        let host = req
            .headers()
            .get(http::header::HOST)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        // Prefer the matched route template over the raw URI path.
        let raw_path = req.uri().path().to_string();
        let path = req
            .extensions()
            .get::<MatchedPath>()
            .map(|p| p.as_str().to_string())
            .unwrap_or_else(|| raw_path.clone());

        // Continue an incoming distributed trace.
        let headers = req.headers().clone();
        let trace = propagation::extract(|name| headers.get(name).and_then(|v| v.to_str().ok()));

        if let Some(tid) = &trace.trace_id {
            hub.configure_scope(|scope| scope.set_trace_id(Some(tid.clone())));
        }

        let span = if self.start_transaction {
            Some(Span::continued(
                "http.server",
                format!("{method} {path}"),
                trace.trace_id.clone(),
                trace.parent_span_id.clone(),
            ))
        } else {
            None
        };

        let started = Instant::now();
        let mut inner = self.inner.clone();
        let hub2 = hub.clone();
        let path2 = path.clone();
        let method2 = method.clone();
        let host2 = host.clone();
        let req_id = trace.request_id.clone();

        Box::pin(async move {
            let fut = Hub::run(hub.clone(), || inner.call(req));
            let result = fut.await;

            let status_code = match &result {
                Ok(resp) => resp.status().as_u16(),
                Err(_) => 500,
            };

            let duration_ms = started.elapsed().as_millis() as u64;

            // Mark the request span / session errored on a 5xx.
            if let Some(mut span) = span {
                if status_code >= 500 {
                    span.set_status("internal_error");
                    Hub::run(hub2.clone(), || {
                        hub2.mark_session_crashed();
                    });
                } else {
                    span.set_status("ok");
                }
                Hub::run(hub2.clone(), || span.finish());
            }

            // Record the request.
            let record = HttpRequestRecord {
                trace_id: trace.trace_id.clone(),
                request_id: req_id,
                direction: "inbound".to_string(),
                method: method2,
                host: host2,
                path: path2,
                status_code,
                duration_ms,
                request_size: None,
                response_size: None,
                user_id: None,
                error_fingerprint: None,
                timestamp: util::now_iso8601(),
            };
            if let Some(client) = hub2.client() {
                client.capture_http_request(record);
            }

            result
        })
    }
}

/// Convenience: a layer wrapped in an [`Arc`] for sharing across routers.
pub fn layer() -> Arc<AllstakLayer> {
    Arc::new(AllstakLayer::new())
}
