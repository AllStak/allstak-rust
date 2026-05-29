//! Distributed-trace propagation header parsing and stamping.

/// Header names the SDK reads for an inbound trace id.
pub const TRACE_ID_HEADERS: [&str; 2] = ["x-allstak-trace-id", "x-trace-id"];
/// Header names the SDK reads for an inbound request id.
pub const REQUEST_ID_HEADERS: [&str; 2] = ["x-request-id", "x-allstak-request-id"];
/// W3C trace context header.
pub const TRACEPARENT: &str = "traceparent";

/// Header the SDK stamps outbound with the trace id.
pub const OUT_TRACE_ID: &str = "X-AllStak-Trace-Id";
/// Header the SDK stamps outbound with the request id.
pub const OUT_REQUEST_ID: &str = "X-AllStak-Request-Id";

/// Resolved trace context extracted from request headers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TraceContext {
    /// Trace id (32-char lower-hex when from W3C).
    pub trace_id: Option<String>,
    /// Parent span id (16-char lower-hex when from W3C).
    pub parent_span_id: Option<String>,
    /// Request id.
    pub request_id: Option<String>,
    /// Raw baggage header value, if any.
    pub baggage: Option<String>,
}

/// Look up a header value by lower-cased name using a getter closure.
fn first<'a, F>(names: &[&str], get: &F) -> Option<String>
where
    F: Fn(&str) -> Option<&'a str>,
{
    for name in names {
        if let Some(v) = get(name) {
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// Parse a W3C `traceparent` value: `00-<trace>-<span>-<flags>`.
fn parse_traceparent(value: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = value.trim().split('-').collect();
    if parts.len() == 4 && parts[1].len() == 32 && parts[2].len() == 16 {
        Some((parts[1].to_string(), parts[2].to_string()))
    } else {
        None
    }
}

/// Extract a [`TraceContext`] from request headers.
///
/// `get` should return the header value for a lower-cased header name.
pub fn extract<'a, F>(get: F) -> TraceContext
where
    F: Fn(&str) -> Option<&'a str>,
{
    let mut ctx = TraceContext::default();

    // W3C traceparent takes precedence for trace + parent span.
    if let Some(tp) = get(TRACEPARENT) {
        if let Some((trace, span)) = parse_traceparent(tp) {
            ctx.trace_id = Some(trace);
            ctx.parent_span_id = Some(span);
        }
    }
    if ctx.trace_id.is_none() {
        ctx.trace_id = first(&TRACE_ID_HEADERS, &get);
    }
    ctx.request_id = first(&REQUEST_ID_HEADERS, &get);
    ctx.baggage = get("baggage").map(|s| s.to_string());
    ctx
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn getter(map: HashMap<&'static str, &'static str>) -> impl Fn(&str) -> Option<&'static str> {
        move |name: &str| map.get(name).copied()
    }

    #[test]
    fn reads_allstak_trace_header() {
        let g = getter(HashMap::from([("x-allstak-trace-id", "abc123")]));
        let ctx = extract(g);
        assert_eq!(ctx.trace_id.as_deref(), Some("abc123"));
    }

    #[test]
    fn parses_traceparent() {
        let g = getter(HashMap::from([(
            "traceparent",
            "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01",
        )]));
        let ctx = extract(g);
        assert_eq!(
            ctx.trace_id.as_deref(),
            Some("0af7651916cd43dd8448eb211c80319c")
        );
        assert_eq!(ctx.parent_span_id.as_deref(), Some("b7ad6b7169203331"));
    }

    #[test]
    fn reads_request_id_fallback() {
        let g = getter(HashMap::from([("x-allstak-request-id", "req-9")]));
        let ctx = extract(g);
        assert_eq!(ctx.request_id.as_deref(), Some("req-9"));
    }
}
