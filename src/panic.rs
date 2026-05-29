//! Panic integration.
//!
//! Installs a global panic hook (chaining the previously-registered hook) that
//! turns a panic into a fatal-level event with a backtrace, marks the active
//! session crashed and flushes before the process exits.

use std::panic::PanicHookInfo;
use std::sync::{Arc, Mutex, Once};

use crate::integration::Integration;
use crate::options::ClientOptions;
use crate::protocol::ErrorEvent;

type Extractor = Box<dyn Fn(&PanicHookInfo<'_>) -> Option<ErrorEvent> + Send + Sync>;

static INSTALL: Once = Once::new();
static EXTRACTORS: Mutex<Vec<Extractor>> = Mutex::new(Vec::new());

/// The default-on panic integration.
#[derive(Default)]
pub struct PanicIntegration {
    extractors: Mutex<Vec<Extractor>>,
}

impl PanicIntegration {
    /// New integration with no custom extractors.
    pub fn new() -> Self {
        PanicIntegration::default()
    }

    /// Register a custom extractor. The first extractor returning `Some` wins;
    /// otherwise the default event builder is used.
    pub fn add_extractor<F>(self, f: F) -> Self
    where
        F: Fn(&PanicHookInfo<'_>) -> Option<ErrorEvent> + Send + Sync + 'static,
    {
        if let Ok(mut v) = self.extractors.lock() {
            v.push(Box::new(f));
        }
        self
    }
}

impl Integration for PanicIntegration {
    fn name(&self) -> &'static str {
        "panic"
    }

    fn setup(&self, _options: &mut ClientOptions) {
        // Move any custom extractors into the global table, then install once.
        if let (Ok(mut local), Ok(mut global)) = (self.extractors.lock(), EXTRACTORS.lock()) {
            global.append(&mut local);
        }
        register_panic_hook();
    }
}

/// Extract the panic message string from hook info.
pub fn message_from_panic_info<'a>(info: &'a PanicHookInfo<'_>) -> &'a str {
    match info.payload().downcast_ref::<&str>() {
        Some(s) => s,
        None => match info.payload().downcast_ref::<String>() {
            Some(s) => s.as_str(),
            None => "Box<dyn Any>",
        },
    }
}

/// Build (or extract) and capture the event for a panic.
pub fn panic_handler(info: &PanicHookInfo<'_>) {
    let event = build_event(info);
    let hub = crate::hub::Hub::current();
    hub.mark_session_crashed();
    hub.capture_event(event);
    // Ensure delivery before a process-ending panic unwinds.
    hub.flush(std::time::Duration::from_secs(2));
}

fn build_event(info: &PanicHookInfo<'_>) -> ErrorEvent {
    if let Ok(extractors) = EXTRACTORS.lock() {
        for extractor in extractors.iter() {
            if let Some(event) = extractor(info) {
                return event;
            }
        }
    }
    let message = message_from_panic_info(info);
    let location = info
        .location()
        .map(|l| format!(" at {}:{}", l.file(), l.line()))
        .unwrap_or_default();
    let full = format!("{message}{location}");

    let opts = ClientOptions::default();
    let frames = crate::backtrace::current_frames(&opts);
    crate::event::event_from_panic(&full, frames)
}

/// Install the global panic hook, chaining the prior hook. Idempotent.
pub fn register_panic_hook() {
    INSTALL.call_once(|| {
        let prev = std::panic::take_hook();
        let prev: Arc<dyn Fn(&PanicHookInfo<'_>) + Sync + Send> = Arc::from(prev);
        std::panic::set_hook(Box::new(move |info| {
            panic_handler(info);
            // Preserve normal panic output.
            prev(info);
        }));
    });
}
