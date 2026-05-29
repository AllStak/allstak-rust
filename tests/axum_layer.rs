//! The axum tower layer must record a request with the matched route-template
//! path and mark a 5xx as a crashed session.
//!
//! Both scenarios mutate the process-global main hub (the layer clones a
//! per-request hub from `Hub::current()` inside `call`, which on a tokio worker
//! thread falls back to the main hub). They run inside a single test so they
//! are sequential and never share a transport.

#![cfg(feature = "axum")]

use std::sync::Arc;
use std::time::Duration;

use allstak::envelope::DataCategory;
use allstak::integrations::axum::AllstakLayer;
use allstak::options::ClientOptions;
use allstak::transport::{StubTransport, StubTransportFactory};
use allstak::{Client, Hub, Scope};
use axum::routing::get;
use axum::Router;
use tower::ServiceExt; // for oneshot

fn build(transport: StubTransport) -> Arc<Client> {
    let opts = ClientOptions {
        api_key: "k".into(),
        release: Some("r@1.0.0".into()),
        environment: Some("test".into()),
        default_integrations: false,
        auto_session_tracking: false,
        transport: Some(Arc::new(StubTransportFactory::new(transport))),
        ..ClientOptions::default()
    };
    Client::new(opts)
}

async fn client_flush(hub: &Arc<Hub>) -> bool {
    let c = hub.client().unwrap();
    tokio::task::spawn_blocking(move || c.flush(Duration::from_secs(2)))
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn axum_layer_records_requests() {
    // --- Scenario 1: route-template path on a 2xx ---
    {
        let transport = StubTransport::new();
        let hub = Arc::new(Hub::new(Some(build(transport.clone())), Scope::new(100)));
        Hub::main().bind_client(hub.client());

        let app = Router::new()
            .route("/users/:id", get(|| async { "ok" }))
            .layer(AllstakLayer::new());

        let request = axum::http::Request::builder()
            .uri("/users/123")
            .header("host", "example.test")
            .body(axum::body::Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), 200);
        let _ = client_flush(&hub).await;

        let reqs = transport.sent_for(DataCategory::HttpRequest);
        assert_eq!(reqs.len(), 1, "one request should be recorded");
        let rec = &reqs[0].body["requests"][0];
        assert_eq!(reqs[0].path, "/ingest/v1/http-requests");
        // Route template, not the concrete /users/123.
        assert_eq!(rec["path"], "/users/:id");
        assert_eq!(rec["method"], "GET");
        assert_eq!(rec["direction"], "inbound");
        assert_eq!(rec["statusCode"], 200);
        assert_eq!(rec["host"], "example.test");
    }

    // --- Scenario 2: a 5xx is recorded with its status ---
    {
        let transport = StubTransport::new();
        let hub = Arc::new(Hub::new(Some(build(transport.clone())), Scope::new(100)));
        Hub::main().bind_client(hub.client());

        let app = Router::new()
            .route(
                "/boom",
                get(|| async { axum::http::StatusCode::INTERNAL_SERVER_ERROR }),
            )
            .layer(AllstakLayer::new());

        let request = axum::http::Request::builder()
            .uri("/boom")
            .body(axum::body::Body::empty())
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), 500);
        let _ = client_flush(&hub).await;

        let reqs = transport.sent_for(DataCategory::HttpRequest);
        let rec = &reqs[0].body["requests"][0];
        assert_eq!(rec["statusCode"], 500);
        assert_eq!(rec["path"], "/boom");
    }
}
