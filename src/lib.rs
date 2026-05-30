//! # allstak
//!
//! Native Rust SDK for the AllStak observability platform. It implements the
//! AllStak ingest wire protocol directly — error monitoring, performance
//! tracing, structured logging and release-health sessions — with a familiar
//! Hub / Scope / Client model.
//!
//! ## Quick start
//!
//! ```no_run
//! let _guard = allstak::init(allstak::ClientOptions {
//!     api_key: "<your-key>".into(),
//!     release: Some("my-service@1.0.0".into()),
//!     environment: Some("production".into()),
//!     ..Default::default()
//! });
//!
//! allstak::configure_scope(|scope| {
//!     scope.set_tag("region", "eu-central");
//! });
//!
//! allstak::capture_message("service started", allstak::Level::Info);
//! // `_guard` flushes pending events when it drops.
//! ```
//!
//! For the least possible configuration, [`init_from_env`] reads options from
//! the environment and installs the panic hook plus (on the `tracing` feature)
//! the global tracing subscriber so logs, spans and database queries are
//! captured automatically.
//!
//! ## Features
//!
//! - `panic` (default): install a global panic hook that captures crashes.
//! - `tracing`: a `tracing_subscriber` layer bridging events and spans.
//! - `axum`: a tower layer that records inbound requests and handler errors.
//! - `actix`: actix-web middleware with the same behavior.
//! - `reqwest-middleware`: outbound HTTP auto-instrumentation — an
//!   `http.client` span, trace-context header injection, and an outbound HTTP
//!   request record, per call, with no per-call code.
//! - `sqlx`: database auto-instrumentation — a `tracing` layer that turns
//!   sqlx's query telemetry into DB query records tied to the active span.
//! - `anyhow`: capture an [`anyhow::Error`] chain.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod backtrace;
mod client;
pub mod db;
pub mod diagnostics;
pub mod envelope;
mod event;
mod hub;
mod integration;
pub mod options;
mod performance;
pub mod propagation;
pub mod protocol;
mod scope;
pub mod scrub;
mod session;
pub mod transport;
pub mod util;

#[cfg(feature = "panic")]
pub mod panic;

/// Feature-gated framework and logging integrations.
pub mod integrations {
    #[cfg(feature = "actix")]
    pub mod actix;
    #[cfg(feature = "axum")]
    pub mod axum;
    #[cfg(feature = "reqwest-middleware")]
    pub mod reqwest;
    #[cfg(feature = "sqlx")]
    pub mod sqlx;
    #[cfg(feature = "tracing")]
    pub mod tracing;
}

use std::sync::Arc;
use std::time::Duration;

pub use client::Client;
pub use db::{capture_query as capture_db_query, normalize_query, query_hash, query_type};
pub use diagnostics::Diagnostics;
pub use hub::{last_event_id, Hub, ScopeGuard};
pub use integration::Integration;
pub use options::{ClientOptions, IntoClientOptions, SessionMode, DEFAULT_HOST};
pub use performance::{start_span, start_transaction, Span};
pub use protocol::{
    Breadcrumb, ErrorEvent, Frame, Heartbeat, Level, RequestContext, SessionStatus, User,
};
pub use scope::Scope;
pub use session::Session;

#[cfg(feature = "reqwest-middleware")]
pub use integrations::reqwest::{
    instrumented_client as instrumented_http_client,
    instrumented_client_from as instrumented_http_client_from, AllstakHttpMiddleware,
};

#[cfg(feature = "anyhow")]
pub use crate::anyhow_support::capture_anyhow;

use uuid::Uuid;

/// RAII guard returned by [`init`]. Flushes pending events and ends the active
/// application session when dropped.
pub struct ClientInitGuard {
    client: Arc<Client>,
    shutdown_timeout: Duration,
    end_session_on_drop: bool,
}

impl ClientInitGuard {
    /// The client this guard manages.
    pub fn client(&self) -> &Arc<Client> {
        &self.client
    }

    /// Whether the client is enabled (has a live transport).
    pub fn is_enabled(&self) -> bool {
        self.client.is_enabled()
    }

    /// Flush pending events, waiting up to the shutdown timeout.
    pub fn flush(&self) -> bool {
        self.client.flush(self.shutdown_timeout)
    }
}

impl Drop for ClientInitGuard {
    fn drop(&mut self) {
        if self.end_session_on_drop {
            Hub::main().end_session();
        }
        self.client.close(self.shutdown_timeout);
    }
}

/// Initialize the SDK and bind a client to the main hub.
///
/// Accepts a bare api key (`&str`/`String`), a `(api_key, ClientOptions)`
/// tuple, or a fully-built [`ClientOptions`]. Returns a [`ClientInitGuard`]
/// that flushes on drop.
pub fn init(opts: impl IntoClientOptions) -> ClientInitGuard {
    let mut options = opts.into_client_options();

    // ALLSTAK_API_KEY / ALLSTAK_DSN env fallback.
    if options.api_key.is_empty() {
        if let Ok(key) = std::env::var("ALLSTAK_API_KEY") {
            options.api_key = key;
        } else if let Ok(dsn) = std::env::var("ALLSTAK_DSN") {
            options.api_key = dsn;
        }
    }

    // Register default integrations.
    if options.default_integrations {
        #[cfg(feature = "panic")]
        {
            let already = options.integrations.iter().any(|i| i.name() == "panic");
            if !already {
                options
                    .integrations
                    .push(Arc::new(panic::PanicIntegration::new()));
            }
        }
    }

    let shutdown_timeout = options.shutdown_timeout;
    let auto_session =
        options.auto_session_tracking && matches!(options.session_mode, SessionMode::Application);

    let client = Client::new(options);
    hub::bind_main_client(client.clone());

    if auto_session {
        Hub::main().start_session();
    }

    ClientInitGuard {
        client,
        shutdown_timeout,
        end_session_on_drop: auto_session,
    }
}

/// Zero-config initialization from the process environment.
///
/// Reads `ALLSTAK_API_KEY` / `ALLSTAK_DSN`, `ALLSTAK_RELEASE`,
/// `ALLSTAK_ENVIRONMENT`, `ALLSTAK_SERVER_NAME`, `ALLSTAK_DEBUG`,
/// `ALLSTAK_SAMPLE_RATE` and `ALLSTAK_SEND_DEFAULT_PII`, then calls [`init`] —
/// which installs the default integrations (the panic hook on the `panic`
/// feature). When the `tracing` feature is enabled it also installs the global
/// `tracing` subscriber (the AllStak `tracing` layer, plus the `sqlx` DB-query
/// layer when that feature is on) so logs, spans and database queries are
/// captured automatically with no further wiring.
///
/// The `tracing` subscriber is installed best-effort: if a global subscriber is
/// already set this is a no-op for the subscriber (the client still initializes
/// and the panic hook is still installed), so it never panics on a double init.
pub fn init_from_env() -> ClientInitGuard {
    let mut options = ClientOptions::default();

    if let Ok(key) = std::env::var("ALLSTAK_API_KEY") {
        options.api_key = key;
    } else if let Ok(dsn) = std::env::var("ALLSTAK_DSN") {
        options.api_key = dsn;
    }
    if let Ok(release) = std::env::var("ALLSTAK_RELEASE") {
        options.release = Some(release);
    }
    if let Ok(env) = std::env::var("ALLSTAK_ENVIRONMENT") {
        options.environment = Some(env);
    }
    if let Ok(name) = std::env::var("ALLSTAK_SERVER_NAME") {
        options.server_name = Some(name);
    }
    if let Ok(debug) = std::env::var("ALLSTAK_DEBUG") {
        options.debug = matches!(debug.to_ascii_lowercase().as_str(), "1" | "true" | "yes");
    }
    if let Ok(rate) = std::env::var("ALLSTAK_SAMPLE_RATE") {
        if let Ok(r) = rate.parse::<f32>() {
            options.sample_rate = r;
        }
    }
    if let Ok(pii) = std::env::var("ALLSTAK_SEND_DEFAULT_PII") {
        options.send_default_pii =
            matches!(pii.to_ascii_lowercase().as_str(), "1" | "true" | "yes");
    }

    let guard = init(options);

    // Best-effort install of the global tracing subscriber so log/span/DB
    // auto-instrumentation needs no extra code. A double-init is tolerated.
    #[cfg(feature = "tracing")]
    install_default_tracing_subscriber();

    guard
}

/// Install the AllStak `tracing` subscriber as the process default, layering in
/// the `sqlx` DB layer when that feature is on. Best-effort: a prior global
/// subscriber is left untouched.
#[cfg(feature = "tracing")]
fn install_default_tracing_subscriber() {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let registry = tracing_subscriber::registry().with(integrations::tracing::layer());

    #[cfg(feature = "sqlx")]
    let result = registry.with(integrations::sqlx::layer()).try_init();
    #[cfg(not(feature = "sqlx"))]
    let result = registry.try_init();

    // Ignore an already-set global subscriber: the client is still live and the
    // panic hook installed; the developer can install their own subscriber.
    let _ = result;
}

// --- Global convenience functions (delegate to the current Hub) ---

/// Capture a pre-built error event.
pub fn capture_event(event: ErrorEvent) -> Uuid {
    Hub::current().capture_event(event)
}

/// Capture a `std::error::Error`.
pub fn capture_error(error: &dyn std::error::Error) -> Uuid {
    Hub::current().capture_error(error)
}

/// Capture a message at `level`.
pub fn capture_message(message: &str, level: Level) -> Uuid {
    Hub::current().capture_message(message, level)
}

/// Build an event from a `std::error::Error` without capturing it.
pub fn event_from_error(error: &dyn std::error::Error) -> ErrorEvent {
    event::event_from_error(error, Hub::current().client().as_deref())
}

/// Mutate the current scope.
pub fn configure_scope<F>(f: F)
where
    F: FnOnce(&mut Scope),
{
    Hub::current().configure_scope(f);
}

/// Run `body` with a temporary scope configured by `scope_fn`.
pub fn with_scope<C, F, R>(scope_fn: C, body: F) -> R
where
    C: FnOnce(&mut Scope),
    F: FnOnce() -> R,
{
    Hub::current().with_scope(scope_fn, body)
}

/// Add a breadcrumb to the current scope.
pub fn add_breadcrumb(breadcrumb: Breadcrumb) {
    Hub::current().add_breadcrumb(breadcrumb);
}

/// Set the active user on the current scope.
pub fn set_user(user: Option<User>) {
    Hub::current().set_user(user);
}

/// Start a release-health session on the current hub.
pub fn start_session() {
    Hub::current().start_session();
}

/// End the active session.
pub fn end_session() {
    Hub::current().end_session();
}

/// End the active session with an explicit status.
pub fn end_session_with_status(status: SessionStatus) {
    Hub::current().end_session_with_status(status);
}

/// Flush the current hub's transport queue.
pub fn flush(timeout: Duration) -> bool {
    Hub::current().flush(timeout)
}

/// Privacy-safe SDK diagnostics. Contains counters and queue sizes only.
pub fn get_diagnostics() -> Diagnostics {
    Hub::current().get_diagnostics()
}

#[cfg(feature = "anyhow")]
mod anyhow_support {
    use uuid::Uuid;

    use crate::hub::Hub;
    use crate::protocol::{ErrorEvent, Level};

    /// Capture an [`anyhow::Error`], walking its chain into the message and
    /// attaching the formatted backtrace lines.
    pub fn capture_anyhow(error: &anyhow::Error) -> Uuid {
        let class = "anyhow::Error".to_string();
        let message = error.to_string();

        // Walk the chain for additional frames.
        let chain: Vec<String> = error.chain().skip(1).map(|c| c.to_string()).collect();
        let mut event = ErrorEvent::new(class, message);
        event.level = Some(Level::Error.as_str().to_string());
        if !chain.is_empty() {
            event.stack_trace = Some(chain);
        }
        Hub::current().capture_event(event)
    }
}
