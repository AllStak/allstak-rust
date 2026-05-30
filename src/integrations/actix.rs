//! actix-web integration.
//!
//! [`Allstak`] is a wrap-able middleware transform. For each request it binds a
//! fresh per-request [`Hub`] (cloned from the top of the stack), continues an
//! incoming distributed trace, records the request to
//! `/ingest/v1/http-requests` (using the matched route pattern as the path),
//! and marks the session crashed on a 5xx response.

use std::future::{ready, Future, Ready};
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context as TaskContext, Poll};
use std::time::Instant;

use actix_web::dev::{Service, ServiceRequest, ServiceResponse, Transform};
use actix_web::Error;

use crate::hub::Hub;
use crate::propagation;
use crate::protocol::HttpRequestRecord;
use crate::util;

/// The actix-web middleware transform. Add with `.wrap(Allstak::new())`.
#[derive(Clone, Default)]
pub struct Allstak {
    capture_server_errors: bool,
}

impl Allstak {
    /// New middleware that captures 5xx responses.
    pub fn new() -> Self {
        Allstak {
            capture_server_errors: true,
        }
    }

    /// Toggle whether 5xx responses mark the session crashed.
    pub fn capture_server_errors(mut self, enable: bool) -> Self {
        self.capture_server_errors = enable;
        self
    }
}

impl<S, B> Transform<S, ServiceRequest> for Allstak
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    B: 'static,
{
    type Response = ServiceResponse<B>;
    type Error = Error;
    type Transform = AllstakMiddleware<S>;
    type InitError = ();
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ready(Ok(AllstakMiddleware {
            service: Rc::new(service),
            capture_server_errors: self.capture_server_errors,
        }))
    }
}

/// The middleware service produced by [`Allstak`].
pub struct AllstakMiddleware<S> {
    service: Rc<S>,
    capture_server_errors: bool,
}

impl<S, B> Service<ServiceRequest> for AllstakMiddleware<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    B: 'static,
{
    type Response = ServiceResponse<B>;
    type Error = Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>>>>;

    fn poll_ready(&self, cx: &mut TaskContext<'_>) -> Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&self, req: ServiceRequest) -> Self::Future {
        let hub = Hub::new_from_top(&Hub::current());

        let method = req.method().to_string();
        let host = req.connection_info().host().to_string();
        let path = req
            .match_pattern()
            .unwrap_or_else(|| req.path().to_string());
        let req_id = req
            .headers()
            .get("x-request-id")
            .or_else(|| req.headers().get("x-allstak-request-id"))
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        let trace = {
            let headers = req.headers();
            propagation::extract(|name| headers.get(name).and_then(|v| v.to_str().ok()))
        };
        if let Some(tid) = &trace.trace_id {
            hub.configure_scope(|scope| scope.set_trace_id(Some(tid.clone())));
        }
        if let Some(rid) = &req_id {
            hub.configure_scope(|scope| scope.set_request_id(Some(rid.clone())));
        }

        let started = Instant::now();
        let service = self.service.clone();
        let capture_errors = self.capture_server_errors;
        let hub2 = hub.clone();
        let trace_id = trace.trace_id.clone();

        Box::pin(async move {
            let fut = service.call(req);
            let res = fut.await;

            let status_code = match &res {
                Ok(r) => r.status().as_u16(),
                Err(_) => 500,
            };
            let duration_ms = started.elapsed().as_millis() as u64;

            if capture_errors && status_code >= 500 {
                hub2.mark_session_crashed();
            }

            let record = HttpRequestRecord {
                trace_id,
                request_id: req_id,
                direction: "inbound".to_string(),
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
            if let Some(client) = hub2.client() {
                client.capture_http_request(record);
            }

            res
        })
    }
}
