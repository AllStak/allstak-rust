//! PII scrubbing must redact CC / SSN / email when send_default_pii is false,
//! and leave them intact when it is true.

mod common;

use allstak::protocol::ErrorEvent;
use common::harness;

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
