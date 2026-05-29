//! Backtrace capture and in-app frame marking.

use crate::options::ClientOptions;
use crate::protocol::Frame;

/// Capture the current backtrace as protocol [`Frame`]s, innermost last.
///
/// `in_app` is resolved from the include/exclude module prefixes in
/// [`ClientOptions`]; with no rules, frames mentioning the crate-local path
/// heuristics are left unset.
pub fn current_frames(options: &ClientOptions) -> Vec<Frame> {
    let bt = backtrace::Backtrace::new();
    let mut frames = Vec::new();

    for frame in bt.frames() {
        for symbol in frame.symbols() {
            let function = symbol.name().map(|n| n.to_string());
            // Skip internal SDK and runtime frames to reduce noise.
            if let Some(name) = &function {
                if name.starts_with("allstak::")
                    || name.starts_with("backtrace::")
                    || name.starts_with("core::panic")
                    || name.starts_with("std::panic")
                    || name.starts_with("std::sys")
                    || name.starts_with("__rust")
                {
                    continue;
                }
            }
            let filename = symbol.filename().map(|p| p.to_string_lossy().into_owned());
            let in_app = function.as_deref().map(|f| is_in_app(f, options));

            frames.push(Frame {
                filename: filename.clone(),
                abs_path: filename,
                function,
                lineno: symbol.lineno(),
                colno: symbol.colno(),
                in_app,
                platform: Some(crate::util::PLATFORM.to_string()),
                debug_id: None,
            });
        }
    }

    // Protocol convention: innermost frame last.
    frames.reverse();
    frames
}

/// Render frames as human-readable `stackTrace` lines.
pub fn frames_as_strings(frames: &[Frame]) -> Vec<String> {
    frames
        .iter()
        .map(|f| {
            let func = f.function.as_deref().unwrap_or("<unknown>");
            match (&f.filename, f.lineno) {
                (Some(file), Some(line)) => format!("{func} ({file}:{line})"),
                (Some(file), None) => format!("{func} ({file})"),
                _ => func.to_string(),
            }
        })
        .collect()
}

fn is_in_app(function: &str, options: &ClientOptions) -> bool {
    for ex in &options.in_app_exclude {
        if function.starts_with(ex.as_str()) {
            return false;
        }
    }
    for inc in &options.in_app_include {
        if function.starts_with(inc.as_str()) {
            return true;
        }
    }
    // Default heuristic: treat well-known runtime/std prefixes as not-in-app.
    !(function.starts_with("std::")
        || function.starts_with("core::")
        || function.starts_with("alloc::")
        || function.starts_with("tokio::")
        || function.starts_with("<core::"))
}
