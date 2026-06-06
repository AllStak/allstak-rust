//! Transport layer.
//!
//! [`Transport`] is the delivery abstraction: capture enqueues an [`Envelope`]
//! and a background worker drains it so capture never blocks. The default
//! implementation is an async reqwest transport driving a dedicated tokio
//! runtime with a bounded queue and retry/backoff. A blocking fallback and an
//! in-memory stub (for tests) are also provided.

use flate2::{write::GzEncoder, Compression};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::envelope::{DataCategory, Envelope};
use crate::options::ClientOptions;

mod ratelimit;
pub use ratelimit::RateLimiter;

/// Delivery sink for envelopes.
pub trait Transport: Send + Sync {
    /// Enqueue an envelope for delivery. Must not block the caller.
    fn send_envelope(&self, envelope: Envelope);

    /// Wait up to `timeout` for the queue to drain. Returns `true` if drained.
    fn flush(&self, timeout: Duration) -> bool;

    /// Drain and stop the worker, waiting up to `timeout`.
    fn shutdown(&self, timeout: Duration) -> bool;

    /// Counter-only diagnostics. Contains no telemetry payload data.
    fn diagnostics(&self) -> TransportDiagnostics {
        TransportDiagnostics::default()
    }
}

/// Privacy-safe transport counters and queue state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TransportDiagnostics {
    /// Envelopes accepted by the transport.
    pub events_captured: u64,
    /// Envelopes successfully sent.
    pub events_sent: u64,
    /// Envelopes that reached a failed terminal outcome.
    pub events_failed: u64,
    /// Envelopes dropped by queue overflow, rate limits, or terminal failure.
    pub events_dropped: u64,
    /// Envelopes persisted to the offline queue for later replay.
    pub events_persisted: u64,
    /// Persisted envelopes replayed successfully.
    pub events_replayed: u64,
    /// Current transport queue size.
    pub queue_size: u64,
    /// Retry attempts performed by the transport.
    pub retry_attempts: u64,
    /// Rate-limited envelope count.
    pub rate_limited_count: u64,
    /// Payloads sent with gzip request compression.
    pub compressed_payloads: u64,
    /// Payloads sent without request compression.
    pub uncompressed_payloads: u64,
    /// Total request bytes saved by compression.
    pub compression_bytes_saved: u64,
    /// Whether the transport has been shut down or disabled.
    pub disabled: bool,
}

#[derive(Default)]
struct TransportCounters {
    events_captured: AtomicU64,
    events_sent: AtomicU64,
    events_failed: AtomicU64,
    events_dropped: AtomicU64,
    events_persisted: AtomicU64,
    events_replayed: AtomicU64,
    queue_size: AtomicU64,
    retry_attempts: AtomicU64,
    rate_limited_count: AtomicU64,
    compressed_payloads: AtomicU64,
    uncompressed_payloads: AtomicU64,
    compression_bytes_saved: AtomicU64,
}

impl TransportCounters {
    fn snapshot(&self, disabled: bool, persistent_queue_size: u64) -> TransportDiagnostics {
        TransportDiagnostics {
            events_captured: self.events_captured.load(Ordering::Relaxed),
            events_sent: self.events_sent.load(Ordering::Relaxed),
            events_failed: self.events_failed.load(Ordering::Relaxed),
            events_dropped: self.events_dropped.load(Ordering::Relaxed),
            events_persisted: self.events_persisted.load(Ordering::Relaxed),
            events_replayed: self.events_replayed.load(Ordering::Relaxed),
            queue_size: self.queue_size.load(Ordering::Relaxed) + persistent_queue_size,
            retry_attempts: self.retry_attempts.load(Ordering::Relaxed),
            rate_limited_count: self.rate_limited_count.load(Ordering::Relaxed),
            compressed_payloads: self.compressed_payloads.load(Ordering::Relaxed),
            uncompressed_payloads: self.uncompressed_payloads.load(Ordering::Relaxed),
            compression_bytes_saved: self.compression_bytes_saved.load(Ordering::Relaxed),
            disabled,
        }
    }
}

/// Builds a [`Transport`] from resolved [`ClientOptions`].
pub trait TransportFactory: Send + Sync {
    /// Create the transport instance for a client.
    fn create_transport(&self, options: &ClientOptions) -> Arc<dyn Transport>;
}

/// Picks the compiled-in default transport (the async reqwest transport).
pub struct DefaultTransportFactory;

impl TransportFactory for DefaultTransportFactory {
    fn create_transport(&self, options: &ClientOptions) -> Arc<dyn Transport> {
        Arc::new(ReqwestTransport::new(options))
    }
}

/// Maximum transient-failure retries per envelope before it is dropped.
const MAX_RETRIES: u32 = 3;
const COMPRESSION_THRESHOLD_BYTES: usize = 1024;

#[derive(Clone)]
struct EventSpool {
    dir: PathBuf,
    max_events: usize,
    max_bytes: u64,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct PersistedEnvelope {
    path: String,
    category: String,
    body: serde_json::Value,
}

impl EventSpool {
    fn from_options(options: &ClientOptions) -> Option<Arc<Self>> {
        if !options.enable_offline_queue {
            return None;
        }
        let dir = options
            .offline_queue_dir
            .clone()
            .unwrap_or_else(|| default_offline_queue_dir(&options.api_key));
        let spool = EventSpool {
            dir,
            max_events: options.offline_queue_max_events.clamp(1, 10_000),
            max_bytes: options.offline_queue_max_bytes.clamp(1, 1024 * 1024 * 1024),
        };
        if spool.ensure_dir().is_err() {
            return None;
        }
        Some(Arc::new(spool))
    }

    fn persist(&self, envelope: &Envelope) -> bool {
        if !is_persistable(envelope.path) || self.ensure_dir().is_err() {
            return false;
        }
        let persisted = PersistedEnvelope {
            path: envelope.path.to_string(),
            category: envelope.category.as_str().to_string(),
            body: envelope.body.clone(),
        };
        let Ok(bytes) = serde_json::to_vec(&persisted) else {
            return false;
        };
        let file = self.dir.join(format!(
            "{}-{}.allstak-spool.json",
            crate::util::now_millis(),
            uuid::Uuid::new_v4().simple()
        ));
        if fs::write(file, bytes).is_err() {
            return false;
        }
        self.enforce_limits();
        true
    }

    fn load_file(&self, path: &Path) -> Option<Envelope> {
        fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<PersistedEnvelope>(&bytes).ok())
            .and_then(|item| item.into_envelope())
    }

    fn remove_file(&self, path: &Path) {
        let _ = fs::remove_file(path);
    }

    fn count(&self) -> u64 {
        self.files().len() as u64
    }

    fn ensure_dir(&self) -> std::io::Result<()> {
        fs::create_dir_all(&self.dir)
    }

    fn enforce_limits(&self) {
        let mut files = self.files_with_sizes();
        while files.len() > self.max_events {
            if let Some((path, _)) = files.first() {
                let _ = fs::remove_file(path);
            }
            files.remove(0);
        }
        let mut total: u64 = files.iter().map(|(_, size)| *size).sum();
        while total > self.max_bytes && !files.is_empty() {
            let (path, size) = files.remove(0);
            let _ = fs::remove_file(path);
            total = total.saturating_sub(size);
        }
    }

    fn files(&self) -> Vec<PathBuf> {
        self.files_with_sizes()
            .into_iter()
            .map(|(path, _)| path)
            .collect()
    }

    fn files_with_sizes(&self) -> Vec<(PathBuf, u64)> {
        let mut out: Vec<(PathBuf, u64)> = fs::read_dir(&self.dir)
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
            .filter_map(|path| {
                let size = fs::metadata(&path).ok()?.len();
                Some((path, size))
            })
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }
}

impl PersistedEnvelope {
    fn into_envelope(self) -> Option<Envelope> {
        Some(Envelope {
            path: known_ingest_path(&self.path)?,
            category: category_from_str(&self.category)?,
            body: self.body,
        })
    }
}

fn default_offline_queue_dir(api_key: &str) -> PathBuf {
    let hash = stable_hash_hex(api_key.as_bytes());
    std::env::temp_dir().join(format!("allstak-rust-spool-{hash}"))
}

fn stable_hash_hex(bytes: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for b in bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn is_persistable(path: &str) -> bool {
    !matches!(
        path,
        "/ingest/v1/sessions/start" | "/ingest/v1/sessions/end"
    )
}

fn known_ingest_path(path: &str) -> Option<&'static str> {
    match path {
        "/ingest/v1/errors" => Some("/ingest/v1/errors"),
        "/ingest/v1/spans" => Some("/ingest/v1/spans"),
        "/ingest/v1/logs" => Some("/ingest/v1/logs"),
        "/ingest/v1/http-requests" => Some("/ingest/v1/http-requests"),
        "/ingest/v1/db" => Some("/ingest/v1/db"),
        "/ingest/v1/heartbeat" => Some("/ingest/v1/heartbeat"),
        "/ingest/v1/releases" => Some("/ingest/v1/releases"),
        "/ingest/v1/sessions/start" => Some("/ingest/v1/sessions/start"),
        "/ingest/v1/sessions/end" => Some("/ingest/v1/sessions/end"),
        _ => None,
    }
}

fn category_from_str(value: &str) -> Option<DataCategory> {
    match value {
        "error" => Some(DataCategory::Error),
        "transaction" => Some(DataCategory::Transaction),
        "session" => Some(DataCategory::Session),
        "log" => Some(DataCategory::Log),
        "http_request" => Some(DataCategory::HttpRequest),
        "db" => Some(DataCategory::Db),
        "heartbeat" => Some(DataCategory::Heartbeat),
        "release" => Some(DataCategory::Release),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Async reqwest transport
// ---------------------------------------------------------------------------

enum Message {
    Send(Envelope),
    Flush(std::sync::mpsc::SyncSender<()>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeliveryOutcome {
    Accepted,
    PermanentFailure,
    RetryableFailure,
}

/// Async transport: a background thread owns a single-threaded tokio runtime
/// that POSTs envelopes with retry/backoff and honours server rate limits.
pub struct ReqwestTransport {
    sender: Mutex<Option<std::sync::mpsc::SyncSender<Message>>>,
    handle: Mutex<Option<std::thread::JoinHandle<()>>>,
    counters: Arc<TransportCounters>,
    spool: Option<Arc<EventSpool>>,
}

impl ReqwestTransport {
    /// Create and start the transport worker.
    pub fn new(options: &ClientOptions) -> Self {
        // Bounded queue: capture stays non-blocking until the queue is full.
        let (tx, rx) = std::sync::mpsc::sync_channel::<Message>(options.transport_queue_size);

        let host = options.host.trim_end_matches('/').to_string();
        let api_key = options.api_key.clone();
        let user_agent = crate::util::user_agent();
        let spool = EventSpool::from_options(options);
        let counters = Arc::new(TransportCounters::default());
        let worker_counters = counters.clone();
        let worker_spool = spool.clone();

        let handle = std::thread::Builder::new()
            .name("allstak-transport".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(_) => return,
                };
                rt.block_on(worker_loop(
                    rx,
                    host,
                    api_key,
                    user_agent,
                    worker_counters,
                    worker_spool,
                ));
            })
            .ok();

        ReqwestTransport {
            sender: Mutex::new(Some(tx)),
            handle: Mutex::new(handle),
            counters,
            spool,
        }
    }
}

async fn worker_loop(
    rx: std::sync::mpsc::Receiver<Message>,
    host: String,
    api_key: String,
    user_agent: String,
    counters: Arc<TransportCounters>,
    spool: Option<Arc<EventSpool>>,
) {
    let client = match reqwest::Client::builder()
        .user_agent(user_agent)
        .timeout(Duration::from_secs(30))
        .build()
    {
        Ok(c) => c,
        Err(_) => return,
    };
    let limiter = RateLimiter::new();

    if let Some(spool) = &spool {
        drain_spool(&client, &host, &api_key, &limiter, &counters, spool).await;
    }

    // This thread owns a dedicated current-thread runtime, so blocking it on
    // the std channel between async deliveries is fine — nothing else runs on
    // it. Each received message's HTTP work is driven by `block_on`.
    loop {
        let msg = match rx.recv() {
            Ok(m) => m,
            Err(_) => break, // all senders dropped
        };
        match msg {
            Message::Send(env) => {
                counters
                    .queue_size
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                        Some(v.saturating_sub(1))
                    })
                    .ok();
                let outcome =
                    deliver(&client, &host, &api_key, &limiter, env.clone(), &counters).await;
                if outcome == DeliveryOutcome::RetryableFailure {
                    persist_or_drop(&spool, &counters, &env);
                }
            }
            Message::Flush(ack) => {
                let _ = ack.send(());
            }
        }
    }
}

async fn drain_spool(
    client: &reqwest::Client,
    host: &str,
    api_key: &str,
    limiter: &RateLimiter,
    counters: &TransportCounters,
    spool: &EventSpool,
) {
    for path in spool.files() {
        let Some(envelope) = spool.load_file(&path) else {
            counters.events_dropped.fetch_add(1, Ordering::Relaxed);
            spool.remove_file(&path);
            continue;
        };
        match deliver(client, host, api_key, limiter, envelope, counters).await {
            DeliveryOutcome::Accepted => {
                counters.events_replayed.fetch_add(1, Ordering::Relaxed);
                spool.remove_file(&path);
            }
            DeliveryOutcome::PermanentFailure => {
                counters.events_dropped.fetch_add(1, Ordering::Relaxed);
                spool.remove_file(&path);
            }
            DeliveryOutcome::RetryableFailure => break,
        }
    }
}

fn persist_or_drop(
    spool: &Option<Arc<EventSpool>>,
    counters: &TransportCounters,
    envelope: &Envelope,
) {
    if let Some(spool) = spool {
        if spool.persist(envelope) {
            counters.events_persisted.fetch_add(1, Ordering::Relaxed);
            return;
        }
    }
    counters.events_dropped.fetch_add(1, Ordering::Relaxed);
}

async fn deliver(
    client: &reqwest::Client,
    host: &str,
    api_key: &str,
    limiter: &RateLimiter,
    env: Envelope,
    counters: &TransportCounters,
) -> DeliveryOutcome {
    if limiter.is_limited(env.category) {
        counters.rate_limited_count.fetch_add(1, Ordering::Relaxed);
        return DeliveryOutcome::RetryableFailure;
    }

    let url = format!("{host}{}", env.path);
    let mut attempt = 0u32;
    let raw_body = match serde_json::to_vec(&env.body) {
        Ok(body) => body,
        Err(_) => {
            counters.events_failed.fetch_add(1, Ordering::Relaxed);
            counters.events_dropped.fetch_add(1, Ordering::Relaxed);
            return DeliveryOutcome::PermanentFailure;
        }
    };
    let prepared = prepare_request_body(raw_body);
    if prepared.compressed {
        counters.compressed_payloads.fetch_add(1, Ordering::Relaxed);
        counters
            .compression_bytes_saved
            .fetch_add(prepared.bytes_saved as u64, Ordering::Relaxed);
    } else {
        counters
            .uncompressed_payloads
            .fetch_add(1, Ordering::Relaxed);
    }

    loop {
        let mut request = client
            .post(&url)
            .header("X-AllStak-Key", api_key)
            .header("Content-Type", "application/json")
            .body(prepared.body.clone());
        if prepared.compressed {
            request = request.header("Content-Encoding", "gzip");
        }
        let res = request.send().await;

        match res {
            Ok(resp) => {
                let status = resp.status();
                if status.as_u16() == 429 {
                    limiter.update_from_response(env.category, resp.headers());
                    counters.events_failed.fetch_add(1, Ordering::Relaxed);
                    counters.rate_limited_count.fetch_add(1, Ordering::Relaxed);
                    return DeliveryOutcome::RetryableFailure;
                }
                if status.is_server_error() {
                    if attempt >= MAX_RETRIES {
                        counters.events_failed.fetch_add(1, Ordering::Relaxed);
                        return DeliveryOutcome::RetryableFailure;
                    }
                    counters.retry_attempts.fetch_add(1, Ordering::Relaxed);
                    if status.as_u16() == 503 {
                        if let Some(delay) = retry_after(resp.headers()) {
                            tokio::time::sleep(delay).await;
                        } else {
                            backoff(attempt).await;
                        }
                    } else {
                        backoff(attempt).await;
                    }
                    attempt += 1;
                    continue;
                }
                // 2xx / non-retryable 4xx: done.
                if status.is_success() {
                    counters.events_sent.fetch_add(1, Ordering::Relaxed);
                } else {
                    counters.events_failed.fetch_add(1, Ordering::Relaxed);
                    counters.events_dropped.fetch_add(1, Ordering::Relaxed);
                }
                return if status.is_success() {
                    DeliveryOutcome::Accepted
                } else {
                    DeliveryOutcome::PermanentFailure
                };
            }
            Err(_) => {
                // Network/transient error: retry with backoff.
                if attempt >= MAX_RETRIES {
                    counters.events_failed.fetch_add(1, Ordering::Relaxed);
                    return DeliveryOutcome::RetryableFailure;
                }
                counters.retry_attempts.fetch_add(1, Ordering::Relaxed);
                backoff(attempt).await;
                attempt += 1;
            }
        }
    }
}

async fn backoff(attempt: u32) {
    // Exponential backoff capped at ~4s: 250ms, 500ms, 1s, ...
    let millis = 250u64.saturating_mul(1 << attempt).min(4000);
    let jitter = crate::util::now_millis() % 125;
    tokio::time::sleep(Duration::from_millis(millis + jitter)).await;
}

fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let value = headers.get("retry-after")?.to_str().ok()?.trim();
    if let Ok(secs) = value.parse::<u64>() {
        return Some(Duration::from_secs(secs.min(300)));
    }
    Some(Duration::from_secs(60))
}

struct PreparedBody {
    body: Vec<u8>,
    compressed: bool,
    bytes_saved: usize,
}

fn prepare_request_body(raw: Vec<u8>) -> PreparedBody {
    if raw.len() < COMPRESSION_THRESHOLD_BYTES {
        return PreparedBody {
            body: raw,
            compressed: false,
            bytes_saved: 0,
        };
    }
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    if encoder.write_all(&raw).is_err() {
        return PreparedBody {
            body: raw,
            compressed: false,
            bytes_saved: 0,
        };
    }
    let compressed = match encoder.finish() {
        Ok(body) => body,
        Err(_) => {
            return PreparedBody {
                body: raw,
                compressed: false,
                bytes_saved: 0,
            };
        }
    };
    if compressed.len() >= raw.len() {
        return PreparedBody {
            body: raw,
            compressed: false,
            bytes_saved: 0,
        };
    }
    let bytes_saved = raw.len() - compressed.len();
    PreparedBody {
        body: compressed,
        compressed: true,
        bytes_saved,
    }
}

impl Transport for ReqwestTransport {
    fn send_envelope(&self, envelope: Envelope) {
        self.counters
            .events_captured
            .fetch_add(1, Ordering::Relaxed);
        if let Ok(guard) = self.sender.lock() {
            if let Some(tx) = guard.as_ref() {
                // Keep capture non-blocking; overflow goes to the persistent
                // offline queue when available so it is not silently lost.
                match tx.try_send(Message::Send(envelope)) {
                    Ok(()) => {
                        self.counters.queue_size.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(std::sync::mpsc::TrySendError::Full(Message::Send(env)))
                    | Err(std::sync::mpsc::TrySendError::Disconnected(Message::Send(env))) => {
                        persist_or_drop(&self.spool, &self.counters, &env);
                    }
                    Err(_) => {
                        self.counters.events_dropped.fetch_add(1, Ordering::Relaxed);
                    }
                }
            } else {
                persist_or_drop(&self.spool, &self.counters, &envelope);
            }
        } else {
            persist_or_drop(&self.spool, &self.counters, &envelope);
        }
    }

    fn flush(&self, timeout: Duration) -> bool {
        let tx = match self.sender.lock() {
            Ok(g) => g.as_ref().cloned(),
            Err(_) => None,
        };
        let Some(tx) = tx else {
            return true;
        };
        let (ack_tx, ack_rx) = std::sync::mpsc::sync_channel::<()>(0);
        if tx.send(Message::Flush(ack_tx)).is_err() {
            return true; // worker gone, nothing pending
        }
        ack_rx.recv_timeout(timeout).is_ok()
    }

    fn shutdown(&self, timeout: Duration) -> bool {
        let drained = self.flush(timeout);
        // Drop the sender so the worker loop exits, then join it.
        if let Ok(mut guard) = self.sender.lock() {
            guard.take();
        }
        if let Ok(mut h) = self.handle.lock() {
            if let Some(handle) = h.take() {
                let _ = handle.join();
            }
        }
        drained
    }

    fn diagnostics(&self) -> TransportDiagnostics {
        let disabled = self.sender.lock().ok().map(|g| g.is_none()).unwrap_or(true);
        let persistent_queue_size = self.spool.as_ref().map(|spool| spool.count()).unwrap_or(0);
        self.counters.snapshot(disabled, persistent_queue_size)
    }
}

impl Drop for ReqwestTransport {
    fn drop(&mut self) {
        self.shutdown(Duration::from_secs(2));
    }
}

// ---------------------------------------------------------------------------
// Stub transport (tests / disabled clients)
// ---------------------------------------------------------------------------

/// In-memory transport that records every envelope. Used by the disabled
/// client and by tests that assert on payload shape without a network.
#[derive(Clone, Default)]
pub struct StubTransport {
    sent: Arc<Mutex<Vec<Envelope>>>,
    counters: Arc<TransportCounters>,
}

impl StubTransport {
    /// Create an empty stub transport.
    pub fn new() -> Self {
        StubTransport::default()
    }

    /// Snapshot of all envelopes captured so far.
    pub fn sent(&self) -> Vec<Envelope> {
        self.sent.lock().map(|v| v.clone()).unwrap_or_default()
    }

    /// Envelopes captured for a single data category.
    pub fn sent_for(&self, category: DataCategory) -> Vec<Envelope> {
        self.sent()
            .into_iter()
            .filter(|e| e.category == category)
            .collect()
    }
}

impl Transport for StubTransport {
    fn send_envelope(&self, envelope: Envelope) {
        self.counters
            .events_captured
            .fetch_add(1, Ordering::Relaxed);
        if let Ok(mut v) = self.sent.lock() {
            v.push(envelope);
            self.counters.events_sent.fetch_add(1, Ordering::Relaxed);
        } else {
            self.counters.events_dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn flush(&self, _timeout: Duration) -> bool {
        true
    }

    fn shutdown(&self, _timeout: Duration) -> bool {
        true
    }

    fn diagnostics(&self) -> TransportDiagnostics {
        self.counters.snapshot(false, 0)
    }
}

/// Factory that hands out clones of a shared [`StubTransport`].
pub struct StubTransportFactory {
    transport: StubTransport,
}

impl StubTransportFactory {
    /// Wrap an existing stub transport so a client can be pointed at it.
    pub fn new(transport: StubTransport) -> Self {
        StubTransportFactory { transport }
    }
}

impl TransportFactory for StubTransportFactory {
    fn create_transport(&self, _options: &ClientOptions) -> Arc<dyn Transport> {
        Arc::new(self.transport.clone())
    }
}

// Internal helper so the rate limiter can be tested without a live transport.
pub(crate) type LimitMap = HashMap<DataCategory, Instant>;
