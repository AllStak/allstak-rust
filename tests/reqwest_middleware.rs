//! The outbound HTTP middleware must, with no per-call code:
//! - inject `traceparent` + `X-AllStak-*` into the request the target receives,
//! - record an outbound HttpRequestRecord (direction `outbound`), and
//! - emit an `http.client` span under the active trace.
//!
//! The target is a wiremock server (never the real network). The ingest side
//! uses an in-memory stub transport so we can assert on payload shape.

#![cfg(feature = "reqwest-middleware")]

use std::sync::Arc;
use std::time::Duration;

use allstak::envelope::DataCategory;
use allstak::options::ClientOptions;
use allstak::transport::{StubTransport, StubTransportFactory};
use allstak::{Client, Hub, Scope};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn build(transport: StubTransport) -> Arc<Client> {
    let opts = ClientOptions {
        api_key: "k".into(),
        release: Some("r@1.0.0".into()),
        environment: Some("test".into()),
        server_name: Some("svc".into()),
        default_integrations: false,
        auto_session_tracking: false,
        transport: Some(Arc::new(StubTransportFactory::new(transport))),
        ..ClientOptions::default()
    };
    Client::new(opts)
}

#[tokio::test(flavor = "multi_thread")]
async fn outbound_request_is_traced_recorded_and_propagated() {
    let target = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/downstream"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&target)
        .await;

    let transport = StubTransport::new();
    let hub = Arc::new(Hub::new(Some(build(transport.clone())), Scope::new(100)));
    // The middleware reads Hub::current(); on a tokio worker thread that falls
    // back to the main hub. Seed an active trace/span so injection has context.
    Hub::main().bind_client(hub.client());
    Hub::main().configure_scope(|s| {
        s.set_trace_id(Some("0af7651916cd43dd8448eb211c80319c".into()));
        s.set_span_id(Some("b7ad6b7169203331".into()));
        s.set_request_id(Some("req-77".into()));
    });

    let client = allstak::instrumented_http_client();
    let url = format!("{}/downstream", target.uri());
    let resp = client.get(&url).send().await.unwrap();
    assert_eq!(resp.status(), 204);

    // The target must have received the injected headers.
    let received: Vec<Request> = target.received_requests().await.unwrap();
    assert_eq!(received.len(), 1);
    let headers = &received[0].headers;
    let traceparent = headers.get("traceparent").unwrap().to_str().unwrap();
    assert!(
        traceparent.starts_with("00-0af7651916cd43dd8448eb211c80319c-"),
        "traceparent carries the active trace id: {traceparent}"
    );
    assert_eq!(
        headers.get("x-allstak-trace-id").unwrap().to_str().unwrap(),
        "0af7651916cd43dd8448eb211c80319c"
    );
    assert_eq!(
        headers
            .get("x-allstak-request-id")
            .unwrap()
            .to_str()
            .unwrap(),
        "req-77"
    );

    // Flush the ingest transport (blocking) off the reactor.
    let c = hub.client().unwrap();
    tokio::task::spawn_blocking(move || c.flush(Duration::from_secs(5)))
        .await
        .unwrap();

    // An outbound HttpRequestRecord was emitted to /ingest/v1/http-requests.
    let reqs = transport.sent_for(DataCategory::HttpRequest);
    assert_eq!(reqs.len(), 1, "one outbound request recorded");
    let rec = &reqs[0].body["requests"][0];
    assert_eq!(reqs[0].path, "/ingest/v1/http-requests");
    assert_eq!(rec["direction"], "outbound");
    assert_eq!(rec["method"], "GET");
    assert_eq!(rec["path"], "/downstream");
    assert_eq!(rec["statusCode"], 204);
    assert_eq!(rec["traceId"], "0af7651916cd43dd8448eb211c80319c");
    assert_eq!(rec["requestId"], "req-77");

    // An http.client span was emitted under the active trace.
    let spans = transport.sent_for(DataCategory::Transaction);
    assert_eq!(spans.len(), 1, "one client span emitted");
    let span = &spans[0].body["spans"][0];
    assert_eq!(span["operation"], "http.client");
    assert_eq!(span["traceId"], "0af7651916cd43dd8448eb211c80319c");
    // Parent is the active server span.
    assert_eq!(span["parentSpanId"], "b7ad6b7169203331");
    assert_eq!(span["status"], "ok");

    // Reset global state for other tests sharing the main hub.
    Hub::main().configure_scope(|s| {
        s.set_trace_id(None);
        s.set_span_id(None);
        s.set_request_id(None);
    });
    Hub::main().bind_client(None);
}
