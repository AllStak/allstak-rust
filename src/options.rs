//! Client configuration.

use std::sync::Arc;

use crate::integration::Integration;
use crate::protocol::ErrorEvent;
use crate::transport::TransportFactory;

/// Default ingest host.
pub const DEFAULT_HOST: &str = "https://api.allstak.sa";

/// How sessions map to units of work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SessionMode {
    /// One session for the whole process lifetime (services / daemons).
    #[default]
    Application,
    /// One session per request / unit of work (web middleware).
    Request,
}

type BeforeSend = Arc<dyn Fn(ErrorEvent) -> Option<ErrorEvent> + Send + Sync>;
type BeforeBreadcrumb =
    Arc<dyn Fn(crate::protocol::Breadcrumb) -> Option<crate::protocol::Breadcrumb> + Send + Sync>;

/// All knobs controlling a [`crate::Client`].
#[derive(Clone)]
pub struct ClientOptions {
    /// Ingest API key (sent as `X-AllStak-Key`).
    pub api_key: String,
    /// Ingest host. Defaults to [`DEFAULT_HOST`].
    pub host: String,
    /// Release identifier (version) for this build.
    pub release: Option<String>,
    /// Deployment environment. Defaults from `debug_assertions`.
    pub environment: Option<String>,
    /// Logical service name attached to spans/logs.
    pub server_name: Option<String>,

    /// Fraction of error events to keep (0.0..=1.0).
    pub sample_rate: f32,
    /// Fraction of transactions to keep when no sampler is set.
    pub traces_sample_rate: f32,

    /// Ring-buffer cap for breadcrumbs.
    pub max_breadcrumbs: usize,
    /// Attach a backtrace to message events even without an error.
    pub attach_stacktrace: bool,
    /// Send identifying PII. When `false`, payloads are value-scrubbed.
    pub send_default_pii: bool,

    /// Module prefixes always marked in-app.
    pub in_app_include: Vec<String>,
    /// Module prefixes always marked not-in-app.
    pub in_app_exclude: Vec<String>,

    /// Final per-event hook; return `None` to drop.
    pub before_send: Option<BeforeSend>,
    /// Per-breadcrumb hook; return `None` to drop.
    pub before_breadcrumb: Option<BeforeBreadcrumb>,

    /// Emit internal debug logging to stderr.
    pub debug: bool,
    /// How long [`crate::ClientInitGuard`] waits to flush on drop.
    pub shutdown_timeout: std::time::Duration,

    /// Open a session at init / end it on shutdown.
    pub auto_session_tracking: bool,
    /// Application- vs request-scoped sessions.
    pub session_mode: SessionMode,

    /// Install the built-in integrations (panic, etc.).
    pub default_integrations: bool,
    /// Additional integrations run in the event pipeline.
    pub integrations: Vec<Arc<dyn Integration>>,

    /// Factory used to build the transport. `None` uses the default.
    pub transport: Option<Arc<dyn TransportFactory>>,

    /// Bounded queue capacity for the transport worker.
    pub transport_queue_size: usize,
}

impl std::fmt::Debug for ClientOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientOptions")
            .field("host", &self.host)
            .field("release", &self.release)
            .field("environment", &self.environment)
            .field("sample_rate", &self.sample_rate)
            .field("traces_sample_rate", &self.traces_sample_rate)
            .field("max_breadcrumbs", &self.max_breadcrumbs)
            .field("send_default_pii", &self.send_default_pii)
            .field("auto_session_tracking", &self.auto_session_tracking)
            .field("session_mode", &self.session_mode)
            .field("integrations", &self.integrations.len())
            .finish_non_exhaustive()
    }
}

impl Default for ClientOptions {
    fn default() -> Self {
        ClientOptions {
            api_key: String::new(),
            host: DEFAULT_HOST.to_string(),
            release: None,
            environment: None,
            server_name: None,
            sample_rate: 1.0,
            traces_sample_rate: 0.0,
            max_breadcrumbs: 100,
            attach_stacktrace: false,
            send_default_pii: false,
            in_app_include: Vec::new(),
            in_app_exclude: Vec::new(),
            before_send: None,
            before_breadcrumb: None,
            debug: false,
            shutdown_timeout: std::time::Duration::from_secs(2),
            auto_session_tracking: true,
            session_mode: SessionMode::Application,
            default_integrations: true,
            integrations: Vec::new(),
            transport: None,
            transport_queue_size: 1000,
        }
    }
}

impl ClientOptions {
    /// Resolved environment, defaulting from `debug_assertions`.
    pub fn resolved_environment(&self) -> String {
        self.environment.clone().unwrap_or_else(|| {
            if cfg!(debug_assertions) {
                "development".to_string()
            } else {
                "production".to_string()
            }
        })
    }
}

/// Anything that can be turned into [`ClientOptions`].
///
/// Accepts a bare api key string, a `(api_key, ClientOptions)` tuple, or an
/// already-built `ClientOptions`.
pub trait IntoClientOptions {
    /// Convert into resolved options.
    fn into_client_options(self) -> ClientOptions;
}

impl IntoClientOptions for ClientOptions {
    fn into_client_options(self) -> ClientOptions {
        self
    }
}

impl IntoClientOptions for &str {
    fn into_client_options(self) -> ClientOptions {
        ClientOptions {
            api_key: self.to_string(),
            ..ClientOptions::default()
        }
    }
}

impl IntoClientOptions for String {
    fn into_client_options(self) -> ClientOptions {
        ClientOptions {
            api_key: self,
            ..ClientOptions::default()
        }
    }
}

impl IntoClientOptions for (&str, ClientOptions) {
    fn into_client_options(self) -> ClientOptions {
        let (api_key, mut opts) = self;
        opts.api_key = api_key.to_string();
        opts
    }
}

impl IntoClientOptions for (String, ClientOptions) {
    fn into_client_options(self) -> ClientOptions {
        let (api_key, mut opts) = self;
        opts.api_key = api_key;
        opts
    }
}
