//! Integration extension point.
//!
//! Integrations register hooks at init and may mutate or drop events as they
//! flow through the capture pipeline.

use crate::options::ClientOptions;
use crate::protocol::ErrorEvent;

/// An installable extension that participates in the event pipeline.
pub trait Integration: Send + Sync {
    /// Stable name for this integration.
    fn name(&self) -> &'static str;

    /// Register hooks at client init. Default: no-op.
    fn setup(&self, _options: &mut ClientOptions) {}

    /// Mutate or drop an event in the pipeline. Default: pass through.
    fn process_event(&self, event: ErrorEvent, _options: &ClientOptions) -> Option<ErrorEvent> {
        Some(event)
    }
}
