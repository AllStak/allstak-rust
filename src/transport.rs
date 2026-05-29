//! Transport layer.
//!
//! [`Transport`] is the delivery abstraction: capture enqueues an [`Envelope`]
//! and a background worker drains it so capture never blocks. The default
//! implementation is an async reqwest transport driving a dedicated tokio
//! runtime with a bounded queue and retry/backoff. A blocking fallback and an
//! in-memory stub (for tests) are also provided.

use std::collections::HashMap;
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

// ---------------------------------------------------------------------------
// Async reqwest transport
// ---------------------------------------------------------------------------

enum Message {
    Send(Envelope),
    Flush(std::sync::mpsc::SyncSender<()>),
}

/// Async transport: a background thread owns a single-threaded tokio runtime
/// that POSTs envelopes with retry/backoff and honours server rate limits.
pub struct ReqwestTransport {
    sender: Mutex<Option<std::sync::mpsc::SyncSender<Message>>>,
    handle: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl ReqwestTransport {
    /// Create and start the transport worker.
    pub fn new(options: &ClientOptions) -> Self {
        // Bounded queue: capture stays non-blocking until the queue is full.
        let (tx, rx) = std::sync::mpsc::sync_channel::<Message>(options.transport_queue_size);

        let host = options.host.trim_end_matches('/').to_string();
        let api_key = options.api_key.clone();
        let user_agent = crate::util::user_agent();

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
                rt.block_on(worker_loop(rx, host, api_key, user_agent));
            })
            .ok();

        ReqwestTransport {
            sender: Mutex::new(Some(tx)),
            handle: Mutex::new(handle),
        }
    }
}

async fn worker_loop(
    rx: std::sync::mpsc::Receiver<Message>,
    host: String,
    api_key: String,
    user_agent: String,
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
                deliver(&client, &host, &api_key, &limiter, env).await;
            }
            Message::Flush(ack) => {
                let _ = ack.send(());
            }
        }
    }
}

async fn deliver(
    client: &reqwest::Client,
    host: &str,
    api_key: &str,
    limiter: &RateLimiter,
    env: Envelope,
) {
    if limiter.is_limited(env.category) {
        return; // honoured rate limit: drop rather than hot-retry
    }

    let url = format!("{host}{}", env.path);
    let mut attempt = 0u32;

    loop {
        let res = client
            .post(&url)
            .header("X-AllStak-Key", api_key)
            .header("Content-Type", "application/json")
            .json(&env.body)
            .send()
            .await;

        match res {
            Ok(resp) => {
                let status = resp.status();
                if status.as_u16() == 429 {
                    limiter.update_from_response(env.category, resp.headers());
                    return;
                }
                if status.is_server_error() {
                    if attempt >= MAX_RETRIES {
                        return;
                    }
                    backoff(attempt).await;
                    attempt += 1;
                    continue;
                }
                // 2xx / non-retryable 4xx: done.
                return;
            }
            Err(_) => {
                // Network/transient error: retry with backoff.
                if attempt >= MAX_RETRIES {
                    return;
                }
                backoff(attempt).await;
                attempt += 1;
            }
        }
    }
}

async fn backoff(attempt: u32) {
    // Exponential backoff capped at ~4s: 250ms, 500ms, 1s, ...
    let millis = 250u64.saturating_mul(1 << attempt).min(4000);
    tokio::time::sleep(Duration::from_millis(millis)).await;
}

impl Transport for ReqwestTransport {
    fn send_envelope(&self, envelope: Envelope) {
        if let Ok(guard) = self.sender.lock() {
            if let Some(tx) = guard.as_ref() {
                // Drop on a full queue rather than blocking capture.
                let _ = tx.try_send(Message::Send(envelope));
            }
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
        if let Ok(mut v) = self.sent.lock() {
            v.push(envelope);
        }
    }

    fn flush(&self, _timeout: Duration) -> bool {
        true
    }

    fn shutdown(&self, _timeout: Duration) -> bool {
        true
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
