//! Integration test against a mock HTTP server (never the real network):
//! verifies the reqwest transport posts to the right path with the auth header
//! and User-Agent, and that the panic hook captures and ships a fatal event.

use flate2::read::GzDecoder;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use allstak::options::ClientOptions;
use allstak::protocol::{ErrorEvent, Level};
use allstak::{Client, Hub, Scope};
use wiremock::matchers::{header, header_exists, method, path};
use wiremock::{Match, Mock, MockServer, Request, ResponseTemplate};

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

fn client_to_host(host: String, spool_dir: Option<PathBuf>) -> std::sync::Arc<Client> {
    let opts = ClientOptions {
        api_key: "secret-key".to_string(),
        host,
        release: Some("itest@1.0.0".to_string()),
        environment: Some("test".to_string()),
        default_integrations: false,
        auto_session_tracking: false,
        offline_queue_dir: spool_dir,
        offline_queue_max_events: 10,
        offline_queue_max_bytes: 1_000_000,
        ..ClientOptions::default()
    };
    Client::new(opts)
}

fn temp_spool_dir() -> PathBuf {
    std::env::temp_dir().join(format!(
        "allstak-rust-transport-test-{}",
        uuid::Uuid::new_v4().simple()
    ))
}

fn spool_files(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .ok()
        .into_iter()
        .flat_map(|entries| entries.filter_map(Result::ok))
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.ends_with(".allstak-spool.json"))
                .unwrap_or(false)
        })
        .collect()
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
async fn tiny_payload_is_sent_uncompressed_and_counted() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/ingest/v1/errors"))
        .and(HeaderAbsent("content-encoding"))
        .and(PlainBodyContains("tiny boom"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let client = client_to(&server);
    client.capture_event(ErrorEvent::new("E", "tiny boom"), &Scope::new(100));

    let c = client.clone();
    assert!(
        tokio::task::spawn_blocking(move || c.flush(Duration::from_secs(5)))
            .await
            .unwrap()
    );
    let diagnostics = client.get_diagnostics();
    assert_eq!(diagnostics.uncompressed_payloads, 1);
    assert_eq!(diagnostics.compressed_payloads, 0);
    assert_eq!(diagnostics.compression_bytes_saved, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn large_payload_is_gzipped_and_counted() {
    let server = MockServer::start().await;
    let message = "x".repeat(8_000);

    Mock::given(method("POST"))
        .and(path("/ingest/v1/errors"))
        .and(header("content-encoding", "gzip"))
        .and(GzipBodyContains(message.clone()))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let client = client_to(&server);
    client.capture_event(ErrorEvent::new("E", message), &Scope::new(100));

    let c = client.clone();
    assert!(
        tokio::task::spawn_blocking(move || c.flush(Duration::from_secs(5)))
            .await
            .unwrap()
    );
    let diagnostics = client.get_diagnostics();
    assert_eq!(diagnostics.compressed_payloads, 1);
    assert_eq!(diagnostics.uncompressed_payloads, 0);
    assert!(diagnostics.compression_bytes_saved > 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn retryable_failure_is_persisted_and_replayed_on_next_init() {
    let spool_dir = temp_spool_dir();
    let offline_client = client_to_host("http://127.0.0.1:9".to_string(), Some(spool_dir.clone()));
    let mut event = ErrorEvent::new("E", "offline persistence event");
    event.metadata = Some(serde_json::json!({
        "token": "should-not-persist",
        "nested": { "apiKey": "should-not-persist" }
    }));
    offline_client.capture_event(event, &Scope::new(100));

    let c = offline_client.clone();
    let _ = tokio::task::spawn_blocking(move || c.flush(Duration::from_secs(8)))
        .await
        .unwrap();

    let diagnostics = offline_client.get_diagnostics();
    assert_eq!(diagnostics.events_failed, 1);
    assert_eq!(diagnostics.events_persisted, 1);
    assert_eq!(diagnostics.events_dropped, 0);
    assert_eq!(diagnostics.queue_size, 1);
    let files = spool_files(&spool_dir);
    assert_eq!(
        files.len(),
        1,
        "retryable failure should leave one persisted envelope"
    );
    let raw = std::fs::read_to_string(&files[0]).unwrap();
    assert!(
        !raw.contains("should-not-persist"),
        "offline queue must store sanitized payloads"
    );

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/ingest/v1/errors"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let replay_client = client_to_host(server.uri(), Some(spool_dir.clone()));
    let c = replay_client.clone();
    assert!(
        tokio::task::spawn_blocking(move || c.flush(Duration::from_secs(8)))
            .await
            .unwrap(),
        "replay transport should flush"
    );
    let replay_diagnostics = replay_client.get_diagnostics();
    assert_eq!(replay_diagnostics.events_replayed, 1);
    assert_eq!(replay_diagnostics.events_sent, 1);
    assert!(
        spool_files(&spool_dir).is_empty(),
        "accepted replay should clear the persisted envelope"
    );

    let _ = std::fs::remove_dir_all(spool_dir);
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

struct HeaderAbsent(&'static str);

impl Match for HeaderAbsent {
    fn matches(&self, request: &Request) -> bool {
        request.headers.get(self.0).is_none()
    }
}

struct PlainBodyContains(&'static str);

impl Match for PlainBodyContains {
    fn matches(&self, request: &Request) -> bool {
        std::str::from_utf8(&request.body)
            .map(|body| body.contains(self.0))
            .unwrap_or(false)
    }
}

struct GzipBodyContains(String);

impl Match for GzipBodyContains {
    fn matches(&self, request: &Request) -> bool {
        let mut decoder = GzDecoder::new(request.body.as_slice());
        let mut body = String::new();
        decoder.read_to_string(&mut body).is_ok() && body.contains(&self.0)
    }
}
