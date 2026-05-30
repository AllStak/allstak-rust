//! Privacy-safe SDK diagnostics.

/// Counter-only SDK diagnostics. This type intentionally contains no payload
/// data, headers, tags, contexts, user fields, or request body snippets.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Diagnostics {
    /// Events accepted into the SDK capture pipeline.
    pub events_captured: u64,
    /// Events successfully sent by the transport.
    pub events_sent: u64,
    /// Events that reached a failed terminal transport outcome.
    pub events_failed: u64,
    /// Events dropped by sampling, hooks, queue overflow, rate limits, or terminal failure.
    pub events_dropped: u64,
    /// Events persisted for later replay.
    pub events_persisted: u64,
    /// Persisted events replayed successfully.
    pub events_replayed: u64,
    /// Current in-memory or persistent queue size.
    pub queue_size: u64,
    /// Transport retry attempts.
    pub retry_attempts: u64,
    /// Rate-limited event count.
    pub rate_limited_count: u64,
    /// Payloads sent with gzip request compression.
    pub compressed_payloads: u64,
    /// Payloads sent without request compression.
    pub uncompressed_payloads: u64,
    /// Total request bytes saved by compression.
    pub compression_bytes_saved: u64,
    /// Sanitizer redaction operations observed in this process.
    pub sanitizer_redaction_count: u64,
    /// Active trace contexts on the current hub.
    pub active_trace_count: u64,
    /// Active span contexts on the current hub.
    pub active_span_count: u64,
    /// Breadcrumb count on the current hub scope.
    pub breadcrumb_count: u64,
    /// Abnormal/crashed session recoveries reported by this SDK.
    pub session_recovery_count: u64,
    /// Whether the SDK/transport is currently disabled.
    pub disabled: bool,
}
