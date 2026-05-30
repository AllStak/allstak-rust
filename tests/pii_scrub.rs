//! PII scrubbing must redact CC / SSN / email when send_default_pii is false,
//! and leave them intact when it is true.

mod common;

use allstak::protocol::ErrorEvent;
use common::harness;
use std::sync::{Arc, Mutex};

fn error_with_secret(secret: &str) -> ErrorEvent {
    ErrorEvent::new("E", secret.to_string())
}

#[test]
fn redacts_email_by_default() {
    let h = harness(|_| {}); // send_default_pii defaults to false
    h.hub
        .capture_event(error_with_secret("contact user@example.com please"));
    let body = common::single_error_body(&h.transport);
    assert_eq!(body["message"], "contact [redacted] please");
}

#[test]
fn redacts_ssn_by_default() {
    let h = harness(|_| {});
    h.hub.capture_event(error_with_secret("ssn 123-45-6789"));
    let body = common::single_error_body(&h.transport);
    assert_eq!(body["message"], "ssn [redacted]");
}

#[test]
fn redacts_credit_card_by_default() {
    let h = harness(|_| {});
    h.hub
        .capture_event(error_with_secret("card 4111 1111 1111 1111 charged"));
    let body = common::single_error_body(&h.transport);
    assert_eq!(body["message"], "card [redacted] charged");
}

#[test]
fn keeps_pii_when_enabled() {
    let h = harness(|opts| opts.send_default_pii = true);
    h.hub
        .capture_event(error_with_secret("contact user@example.com"));
    let body = common::single_error_body(&h.transport);
    assert_eq!(body["message"], "contact user@example.com");
}

#[test]
fn scrubs_nested_metadata() {
    let h = harness(|_| {});
    h.hub.configure_scope(|scope| {
        scope.set_extra("email", serde_json::json!("nested@example.com"));
    });
    h.hub.capture_event(ErrorEvent::new("E", "ok"));
    let body = common::single_error_body(&h.transport);
    assert_eq!(body["metadata"]["extra"]["email"], "[redacted]");
}

#[test]
fn before_send_receives_sanitized_event() {
    let seen = Arc::new(Mutex::new(None));
    let seen_hook = seen.clone();
    let h = harness(|opts| {
        opts.before_send = Some(Arc::new(move |event| {
            *seen_hook.lock().unwrap() = Some(event.clone());
            Some(event)
        }));
    });
    let mut event = error_with_secret("card 4111111111111111");
    event.metadata = Some(serde_json::json!({
        "Authorization": "Bearer abc",
        "nested": { "apiKey": "key-123" }
    }));
    h.hub.capture_event(event);

    let captured = seen.lock().unwrap().clone().expect("before_send called");
    assert_eq!(captured.message, "card [redacted]");
    assert_eq!(
        captured.metadata.as_ref().unwrap()["Authorization"],
        "[redacted]"
    );
    assert_eq!(
        captured.metadata.as_ref().unwrap()["nested"]["apiKey"],
        "[redacted]"
    );
}

#[test]
fn before_send_cannot_reintroduce_secrets() {
    let h = harness(|opts| {
        opts.before_send = Some(Arc::new(|mut event| {
            event.message = "card 4111111111111111".to_string();
            event.metadata = Some(serde_json::json!({
                "Authorization": "Bearer abc",
                "nested": { "token": "secret-token" }
            }));
            Some(event)
        }));
    });
    h.hub.capture_event(error_with_secret("original"));

    let body = common::single_error_body(&h.transport);
    assert_eq!(body["message"], "card [redacted]");
    assert_eq!(body["metadata"]["Authorization"], "[redacted]");
    assert_eq!(body["metadata"]["nested"]["token"], "[redacted]");
}
