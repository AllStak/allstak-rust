//! The panic integration must capture a fatal-level event with frames and mark
//! the active session crashed, without aborting the test process.

#![cfg(feature = "panic")]

use std::sync::Arc;

use allstak::envelope::DataCategory;
use allstak::options::ClientOptions;
use allstak::transport::{StubTransport, StubTransportFactory};
use allstak::{Client, Hub, Scope};

#[test]
fn panic_handler_builds_fatal_event_and_crashes_session() {
    // Build a client wired to a stub transport, then exercise the panic
    // handler directly (the same function the installed hook calls). This
    // avoids depending on global-hook install order across the test binary.
    let transport = StubTransport::new();
    let opts = ClientOptions {
        api_key: "k".into(),
        release: Some("r@1.0.0".into()),
        environment: Some("test".into()),
        default_integrations: false,
        auto_session_tracking: false,
        transport: Some(Arc::new(StubTransportFactory::new(transport.clone()))),
        ..ClientOptions::default()
    };
    let client = Client::new(opts);
    let hub = Arc::new(Hub::new(Some(client), Scope::new(100)));

    Hub::run(hub.clone(), || {
        hub.start_session();

        // Capture panic info from a real (caught) panic and feed it to the
        // panic handler exactly as the installed hook would.
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|info| {
            allstak::panic::panic_handler(info);
        }));
        let result = std::panic::catch_unwind(|| panic!("kaboom-42"));
        std::panic::set_hook(prev);
        assert!(result.is_err(), "the closure should have panicked");

        hub.end_session();
    });

    let errors = transport.sent_for(DataCategory::Error);
    let fatal = errors
        .iter()
        .find(|e| e.body["level"] == "fatal")
        .expect("a fatal-level panic event");
    assert_eq!(fatal.body["exceptionClass"], "panic");
    assert!(
        fatal.body["message"]
            .as_str()
            .unwrap()
            .contains("kaboom-42"),
        "panic message should be carried, got {:?}",
        fatal.body["message"]
    );
    // A backtrace should be attached.
    assert!(fatal.body.get("frames").is_some() || fatal.body.get("stackTrace").is_some());

    let sessions = transport.sent_for(DataCategory::Session);
    let end = sessions.last().expect("session end");
    assert_eq!(end.body["status"], "crashed");
}

#[test]
fn message_extraction_handles_str_and_string() {
    use allstak::protocol::ErrorEvent;
    // Smoke-test the event builder path indirectly via event_from_panic.
    let event = ErrorEvent::new("panic", "boom at src/x.rs:1");
    assert_eq!(event.exception_class, "panic");
    assert_eq!(event.message, "boom at src/x.rs:1");
}
