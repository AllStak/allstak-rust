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

/// Render a W3C `traceparent` value (`00-<trace>-<span>-01`) from a trace id
/// and the span id that becomes the parent of the downstream request.
///
/// `trace_id` is normalised to 32 lower-hex chars and `span_id` to 16, so
/// values minted by [`crate::util::new_trace_id`] / [`crate::util::new_span_id`]
/// are accepted as-is.
pub fn format_traceparent(trace_id: &str, span_id: &str) -> String {
    let trace = normalize_hex(trace_id, 32);
    let span = normalize_hex(span_id, 16);
    format!("00-{trace}-{span}-01")
}

/// Pad/truncate a hex string to exactly `width` lower-hex chars.
fn normalize_hex(value: &str, width: usize) -> String {
    let cleaned: String = value
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    if cleaned.len() >= width {
        cleaned[..width].to_string()
    } else {
        let mut s = String::with_capacity(width);
        for _ in 0..(width - cleaned.len()) {
            s.push('0');
        }
        s.push_str(&cleaned);
        s
    }
}

/// Inject the active trace context into an outbound request via a header
/// setter closure. Complements [`extract`].
///
/// Stamps the W3C `traceparent` plus AllStak's own `X-AllStak-Trace-Id` /
/// `X-AllStak-Request-Id` headers so a downstream AllStak-instrumented service
/// can continue the same trace. `span_id` is the id of the outbound client span
/// and becomes the `parent-id` segment of `traceparent`.
///
/// `set` receives a header name and its value for each header to stamp.
pub fn inject<F>(ctx: &TraceContext, span_id: Option<&str>, mut set: F)
where
    F: FnMut(&str, &str),
{
    if let Some(trace_id) = &ctx.trace_id {
        set(OUT_TRACE_ID, trace_id);
        if let Some(span) = span_id {
            set(TRACEPARENT, &format_traceparent(trace_id, span));
        }
    }
    if let Some(request_id) = &ctx.request_id {
        set(OUT_REQUEST_ID, request_id);
    }
    if let Some(baggage) = &ctx.baggage {
        if !baggage.is_empty() {
            set("baggage", baggage);
        }
    }
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

    #[test]
    fn format_traceparent_normalizes_widths() {
        let tp = format_traceparent("0af7651916cd43dd8448eb211c80319c", "b7ad6b7169203331");
        assert_eq!(tp, "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01");
        // Short ids are left-padded to the right width.
        let tp = format_traceparent("abc", "1");
        assert_eq!(tp, "00-00000000000000000000000000000abc-0000000000000001-01");
    }

    #[test]
    fn inject_round_trips_through_extract() {
        let ctx = TraceContext {
            trace_id: Some("0af7651916cd43dd8448eb211c80319c".to_string()),
            parent_span_id: None,
            request_id: Some("req-42".to_string()),
            baggage: None,
        };
        let mut headers: HashMap<String, String> = HashMap::new();
        inject(&ctx, Some("b7ad6b7169203331"), |name, value| {
            headers.insert(name.to_ascii_lowercase(), value.to_string());
        });

        // traceparent + AllStak headers were stamped.
        assert_eq!(
            headers.get("traceparent").map(String::as_str),
            Some("00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01")
        );
        assert_eq!(
            headers.get("x-allstak-trace-id").map(String::as_str),
            Some("0af7651916cd43dd8448eb211c80319c")
        );
        assert_eq!(
            headers.get("x-allstak-request-id").map(String::as_str),
            Some("req-42")
        );

        // And a downstream extract reconstructs the same trace + parent span.
        let extracted = extract(|name| headers.get(name).map(String::as_str));
        assert_eq!(
            extracted.trace_id.as_deref(),
            Some("0af7651916cd43dd8448eb211c80319c")
        );
        assert_eq!(extracted.parent_span_id.as_deref(), Some("b7ad6b7169203331"));
        assert_eq!(extracted.request_id.as_deref(), Some("req-42"));
    }

    #[test]
    fn inject_without_trace_id_stamps_nothing_traced() {
        let ctx = TraceContext::default();
        let mut count = 0;
        inject(&ctx, Some("b7ad6b7169203331"), |_, _| count += 1);
        assert_eq!(count, 0, "no trace id => no headers");
    }
}
