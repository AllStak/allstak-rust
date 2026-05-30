# allstak

Native Rust SDK for the [AllStak](https://allstak.sa) observability platform.

`allstak` implements the AllStak ingest wire protocol directly — it is not a
wrapper around another agent. It gives you error monitoring, performance
tracing, structured logging and release-health sessions through a familiar
Hub / Scope / Client model, with optional framework integrations behind cargo
feature flags so the base crate stays light.

## Install

```toml
[dependencies]
allstak = "0.1"
```

Enable the integrations you need:

```toml
[dependencies]
allstak = { version = "0.1", features = ["tracing", "axum"] }
```

| Feature              | Default | What it adds |
|----------------------|---------|--------------|
| `panic`              | yes     | Global panic hook that captures crashes as fatal events. |
| `tracing`            | no      | A `tracing-subscriber` layer: events → logs/breadcrumbs/events, spans → spans. |
| `axum`               | no      | A tower `Layer` that records inbound requests and captures handler errors. |
| `actix`              | no      | actix-web middleware with the same behavior. |
| `reqwest-middleware` | no      | Outbound HTTP auto-instrumentation: an `http.client` span, trace-context header injection, and an outbound request record — per call, no per-call code. |
| `sqlx`               | no      | Database auto-instrumentation: a `tracing` layer that turns sqlx query telemetry into DB query records tied to the active span. |
| `anyhow`             | no      | `capture_anyhow` to capture an `anyhow::Error` chain. |

## Zero-config

`init_from_env` reads `ALLSTAK_API_KEY` / `ALLSTAK_DSN`, `ALLSTAK_RELEASE`,
`ALLSTAK_ENVIRONMENT`, `ALLSTAK_SERVER_NAME`, `ALLSTAK_DEBUG`,
`ALLSTAK_SAMPLE_RATE` and `ALLSTAK_SEND_DEFAULT_PII`, installs the panic hook,
and — with the `tracing` feature — also installs the global tracing subscriber
(the log/span layer plus the `sqlx` DB layer when enabled). After one call you
get crash, log, span and database capture with no further wiring:

```rust
fn main() {
    let _guard = allstak::init_from_env();
    // panics, logs, spans and sqlx queries are now captured automatically.
}
```

## Quick start

```rust
fn main() {
    let _guard = allstak::init(allstak::ClientOptions {
        api_key: std::env::var("ALLSTAK_API_KEY").unwrap_or_default(),
        host: "https://api.allstak.sa".into(),
        release: Some("my-service@1.0.0".into()),
        environment: Some("production".into()),
        ..Default::default()
    });

    allstak::configure_scope(|scope| {
        scope.set_tag("region", "eu-central");
        scope.set_user(Some(allstak::User {
            id: Some("user-42".into()),
            ..Default::default()
        }));
    });

    allstak::add_breadcrumb(allstak::Breadcrumb::new("starting work"));

    if let Err(err) = do_work() {
        allstak::capture_error(&*err);
    }

    // `_guard` flushes pending events and ends the session when it drops.
}

fn do_work() -> Result<(), Box<dyn std::error::Error>> {
    Ok(())
}
```

`init` accepts a bare api key (`&str` / `String`), a `(api_key, ClientOptions)`
tuple, or a full `ClientOptions`. It returns a `ClientInitGuard` that flushes
on drop with a configurable `shutdown_timeout` (default 2s). If `api_key` is
empty the SDK reads `ALLSTAK_API_KEY` (then `ALLSTAK_DSN`) from the
environment.

## Capturing

```rust
use allstak::Level;

allstak::capture_message("cache warmed", Level::Info);

let err = std::io::Error::new(std::io::ErrorKind::Other, "disk full");
allstak::capture_error(&err);

allstak::with_scope(
    |scope| scope.set_tag("job", "nightly"),
    || {
        // events captured here also carry the `job` tag
    },
);
```

## Performance tracing

```rust
let tx = allstak::start_transaction("task.run", "nightly-rollup");
{
    let mut child = tx.start_child("db.query", "SELECT ...");
    child.set_tag("db.system", "postgres");
    child.finish();
}
tx.finish(); // both spans are POSTed to /ingest/v1/spans
```

## Release-health sessions

Sessions are tracked automatically when `auto_session_tracking` is on
(default) in `SessionMode::Application`. An error/fatal event marks the active
session errored; a panic marks it crashed. You can also drive them by hand:

```rust
allstak::start_session();
// ... work ...
allstak::end_session_with_status(allstak::SessionStatus::Ok);
```

## tracing integration

```rust
# #[cfg(feature = "tracing")]
# fn demo() {
use tracing_subscriber::prelude::*;

tracing_subscriber::registry()
    .with(allstak::integrations::tracing::layer())
    .init();

tracing::info!(order_id = 7, "order placed");      // -> breadcrumb
tracing::error!(error = "timeout", "request failed"); // -> error event
# }
```

`ERROR` events become AllStak events, `INFO`/`WARN` become breadcrumbs, and
instrumented spans become AllStak spans. Fields prefixed `tags.` are recorded
as tags. Override with `.event_filter(..)` / `.span_filter(..)`.

## axum integration

```rust
# #[cfg(feature = "axum")]
# fn demo() {
use allstak::integrations::axum::AllstakLayer;
use axum::{routing::get, Router};

let app: Router = Router::new()
    .route("/users/:id", get(|| async { "ok" }))
    .layer(AllstakLayer::new());
# let _ = app;
# }
```

The layer binds a fresh per-request hub, continues an inbound distributed trace
(`X-AllStak-Trace-Id` / `X-Trace-Id` / W3C `traceparent`), opens a request
span, and records the request to `/ingest/v1/http-requests` using the matched
route template (`/users/:id`) as the path. A `5xx` marks the request span and
session as failed.

## actix-web integration

```rust
# #[cfg(feature = "actix")]
# fn demo() {
use actix_web::App;
use allstak::integrations::actix::Allstak;

let app = App::new().wrap(Allstak::new());
# let _ = app;
# }
```

## Outbound HTTP (reqwest)

With the `reqwest-middleware` feature, wrap your reqwest client once and every
outbound request opens an `http.client` span, propagates the active trace
context downstream (`traceparent` + `X-AllStak-*`), and records an outbound
request — no per-call code:

```rust
# #[cfg(feature = "reqwest-middleware")]
# async fn demo() -> Result<(), Box<dyn std::error::Error>> {
let client = allstak::instrumented_http_client();
let _ = client.get("https://api.example.com/things").send().await?;
# Ok(())
# }
```

To attach the middleware to a client you already build:

```rust
# #[cfg(feature = "reqwest-middleware")]
# fn demo() -> reqwest_middleware::ClientWithMiddleware {
use reqwest_middleware::ClientBuilder;
use allstak::AllstakHttpMiddleware;

ClientBuilder::new(reqwest::Client::new())
    .with(AllstakHttpMiddleware::new())
    .build()
# }
```

When called inside a request handled by the `axum` / `actix` middleware, the
client span nests under the request span and shares its trace automatically.

## Database (sqlx)

With the `sqlx` feature (which builds on `tracing`), add the DB layer alongside
the tracing layer. Every sqlx statement is then captured as a normalized DB
query record tied to the active span — no per-query code, any sqlx backend:

```rust
# #[cfg(all(feature = "sqlx", feature = "tracing"))]
# fn demo() {
use tracing_subscriber::prelude::*;

tracing_subscriber::registry()
    .with(allstak::integrations::tracing::layer())
    .with(allstak::integrations::sqlx::layer().database_type("postgres"))
    .init();
# }
```

`init_from_env` installs both layers for you when these features are enabled.

## Configuration

`ClientOptions` exposes: `api_key`, `host`, `release`, `environment`,
`server_name`, `sample_rate`, `traces_sample_rate`, `max_breadcrumbs`,
`attach_stacktrace`, `send_default_pii`, `in_app_include` / `in_app_exclude`,
`before_send`, `before_breadcrumb`, `debug`, `shutdown_timeout`,
`auto_session_tracking`, `session_mode`, `default_integrations`,
`integrations`, `transport`, and `transport_queue_size`.

### Privacy

`send_default_pii` defaults to `false`. With it off, every outbound payload is
value-scrubbed: strings that look like an email address, a national insurance /
SSN number, or a payment-card number are replaced with `[redacted]` before the
payload leaves the process. Set `send_default_pii = true` to send identifying
data verbatim.

## Transport

The default transport is an async `reqwest` client running on a dedicated
background thread with a bounded queue, exponential backoff retry on transient
(network / `5xx`) failures, and per-category rate-limit handling for `429`
responses (honouring `Retry-After`). Capture never blocks the calling thread.

Every request carries:

- `X-AllStak-Key: <api_key>`
- `User-Agent: allstak-rust/<version>`

## Wire protocol

All payloads are JSON with camelCase field names. Endpoints (relative to
`host`):

| Endpoint | Purpose |
|----------|---------|
| `POST /ingest/v1/errors` | Error / message / panic events |
| `POST /ingest/v1/spans` | Performance spans / transactions |
| `POST /ingest/v1/logs` | Structured logs |
| `POST /ingest/v1/http-requests` | HTTP request records |
| `POST /ingest/v1/db` | Database query records |
| `POST /ingest/v1/sessions/start` · `.../end` | Release-health sessions |
| `POST /ingest/v1/heartbeat` | Cron / heartbeat check-ins |
| `POST /ingest/v1/releases` | Release registration (best-effort) |

## License

MIT — see [LICENSE](LICENSE).
