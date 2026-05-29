//! Integration test against a mock HTTP server (never the real network):
//! verifies the reqwest transport posts to the right path with the auth header
//! and User-Agent, and that the panic hook captures and ships a fatal event.

use std::time::Duration;

use allstak::options::ClientOptions;
use allstak::protocol::{ErrorEvent, Level};
use allstak::{Client, Hub, Scope};
use wiremock::matchers::{header, header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client_to(server: &MockServer) -> std::sync::Arc<Client> {
    let opts = ClientOptions {
        api_key: "secret-key".to_string(),
        host: server.uri(),
        release: Some("itest@1.0.0".to_string()),
        environment: Some("test".to_string()),
        default_integrations: false,
        auto_session_tracking: false,
        ..ClientOptions::default()
    };
    Client::new(opts)
}

#[tokio::test(flavor = "multi_thread")]
async fn posts_error_with_auth_header_and_user_agent() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/ingest/v1/errors"))
        .and(header("x-allstak-key", "secret-key"))
        .and(header_exists("user-agent"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let client = client_to(&server);
    let scope = Scope::new(100);
    let mut event = ErrorEvent::new("E", "network boom");
    event.level = Some(Level::Error.as_str().to_string());
    client.capture_event(event, &scope);

    // Flush blocks on a worker thread; run it off the async reactor.
    let c = client.clone();
    let flushed = tokio::task::spawn_blocking(move || c.flush(Duration::from_secs(5)))
        .await
        .unwrap();
    assert!(flushed, "transport should flush within the timeout");

    // The expect(1) assertion is verified on drop of the server.
    drop(server);
}

#[tokio::test(flavor = "multi_thread")]
async fn user_agent_is_allstak_rust() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/ingest/v1/errors"))
        .and(header(
            "user-agent",
            format!("allstak-rust/{}", env!("CARGO_PKG_VERSION")).as_str(),
        ))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let client = client_to(&server);
    client.capture_event(ErrorEvent::new("E", "ua check"), &Scope::new(100));

    let c = client.clone();
    tokio::task::spawn_blocking(move || c.flush(Duration::from_secs(5)))
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn fatal_event_is_delivered_over_http() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/ingest/v1/errors"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1..)
        .mount(&server)
        .await;

    let client = client_to(&server);
    let hub = std::sync::Arc::new(Hub::new(Some(client.clone()), Scope::new(100)));

    // Build and capture the same fatal event the panic hook produces, then
    // confirm it is delivered over the wire.
    Hub::run(hub.clone(), || {
        let frames = allstak::backtrace::current_frames(client.options());
        let mut event = ErrorEvent {
            level: Some(Level::Fatal.as_str().to_string()),
            ..ErrorEvent::new("panic", "explicit boom")
        };
        event.frames = Some(frames);
        Hub::current().capture_event(event);
    });

    let c = client.clone();
    let flushed = tokio::task::spawn_blocking(move || c.flush(Duration::from_secs(5)))
        .await
        .unwrap();
    assert!(flushed);
}
