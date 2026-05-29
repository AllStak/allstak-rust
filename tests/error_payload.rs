//! Assert the error payload shape and field names match the ingest contract.

mod common;

use allstak::protocol::{ErrorEvent, Level, User};
use common::{harness, single_error_body};

#[test]
fn error_payload_field_names_match_contract() {
    let h = harness(|_| {});

    let mut event = ErrorEvent::new("std::io::Error", "boom");
    event.level = Some(Level::Error.as_str().to_string());
    event.stack_trace = Some(vec!["frame_a".into(), "frame_b".into()]);
    h.hub.capture_event(event);

    let body = single_error_body(&h.transport);

    // Required contract fields, exact camelCase names.
    assert_eq!(body["exceptionClass"], "std::io::Error");
    assert_eq!(body["message"], "boom");
    assert_eq!(body["level"], "error");
    assert_eq!(body["stackTrace"][0], "frame_a");
    assert_eq!(body["stackTrace"][1], "frame_b");

    // Identity defaults applied by the client.
    assert_eq!(body["environment"], "test");
    assert_eq!(body["release"], "test@1.0.0");
    assert_eq!(body["sdkName"], "rust");
    assert_eq!(body["platform"], "rust");
    assert!(body["sdkVersion"].is_string());

    // event_id must NOT appear on the wire.
    assert!(body.get("eventId").is_none());
    assert!(body.get("event_id").is_none());
}

#[test]
fn capture_error_derives_class_and_walks_chain() {
    use std::error::Error;
    use std::fmt;

    #[derive(Debug)]
    struct Inner;
    impl fmt::Display for Inner {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "inner cause")
        }
    }
    impl Error for Inner {}

    #[derive(Debug)]
    struct Outer(Inner);
    impl fmt::Display for Outer {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "outer failure")
        }
    }
    impl Error for Outer {
        fn source(&self) -> Option<&(dyn Error + 'static)> {
            Some(&self.0)
        }
    }

    let h = harness(|_| {});
    let err = Outer(Inner);
    h.hub.capture_error(&err);

    let body = single_error_body(&h.transport);
    assert_eq!(body["exceptionClass"], "Outer");
    assert_eq!(body["message"], "outer failure: inner cause");
    assert_eq!(body["level"], "error");
}

#[test]
fn capture_message_sets_level() {
    let h = harness(|_| {});
    h.hub.capture_message("just info", Level::Info);
    let body = single_error_body(&h.transport);
    assert_eq!(body["message"], "just info");
    assert_eq!(body["level"], "info");
}

#[test]
fn user_is_serialized_with_contract_fields() {
    let h = harness(|_| {});
    let mut event = ErrorEvent::new("E", "m");
    event.user = Some(User {
        id: Some("u1".into()),
        email: Some("a@b.co".into()),
        ip: Some("1.2.3.4".into()),
    });
    // send_default_pii defaults to false, but a clean-looking id stays.
    h.hub.capture_event(event);
    let body = single_error_body(&h.transport);
    assert_eq!(body["user"]["id"], "u1");
}
