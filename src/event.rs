//! Building [`ErrorEvent`]s from errors, messages and panics.

use crate::client::Client;
use crate::protocol::{ErrorEvent, Frame, Level};

/// Best-effort exception class name from a `std::error::Error`.
///
/// A `&dyn Error` erases the concrete type, so the name is recovered from the
/// `Debug` representation: the leading identifier of `Debug` output is the
/// struct/enum name (e.g. `Outer(Inner)` -> `Outer`). Falls back to `Error`.
fn class_from_error(error: &dyn std::error::Error) -> String {
    let debug = format!("{error:?}");
    let name: String = debug
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if name.is_empty() {
        "Error".to_string()
    } else {
        name
    }
}

/// Build an event from a `std::error::Error`, walking the `source()` chain into
/// the message and attaching a backtrace.
pub fn event_from_error(error: &dyn std::error::Error, client: Option<&Client>) -> ErrorEvent {
    let class = class_from_error(error);

    // Walk the source chain to build a richer message.
    let mut message = error.to_string();
    let mut source = error.source();
    let mut chain = Vec::new();
    while let Some(src) = source {
        chain.push(src.to_string());
        source = src.source();
    }
    if !chain.is_empty() {
        message = format!("{message}: {}", chain.join(": "));
    }

    let mut event = ErrorEvent::new(class, message);
    event.level = Some(Level::Error.as_str().to_string());
    attach_backtrace(&mut event, client);
    event
}

/// Build an event from a plain message at `level`.
pub fn event_from_message(message: &str, level: Level, client: Option<&Client>) -> ErrorEvent {
    let mut event = ErrorEvent::new("Message", message.to_string());
    event.level = Some(level.as_str().to_string());
    if let Some(c) = client {
        if c.options().attach_stacktrace {
            attach_backtrace(&mut event, client);
        }
    }
    event
}

/// Build a fatal panic event with the given message and frames.
pub fn event_from_panic(message: &str, frames: Vec<Frame>) -> ErrorEvent {
    let mut event = ErrorEvent::new("panic", message.to_string());
    event.level = Some(Level::Fatal.as_str().to_string());
    if !frames.is_empty() {
        event.stack_trace = Some(crate::backtrace::frames_as_strings(&frames));
        event.frames = Some(frames);
    }
    event
}

fn attach_backtrace(event: &mut ErrorEvent, client: Option<&Client>) {
    let default_opts;
    let opts = match client {
        Some(c) => c.options(),
        None => {
            default_opts = crate::options::ClientOptions::default();
            &default_opts
        }
    };
    let frames = crate::backtrace::current_frames(opts);
    if !frames.is_empty() {
        event.stack_trace = Some(crate::backtrace::frames_as_strings(&frames));
        event.frames = Some(frames);
    }
}
