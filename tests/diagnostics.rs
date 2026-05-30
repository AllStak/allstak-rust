mod common;

use std::sync::Arc;

use allstak::protocol::{Breadcrumb, ErrorEvent, LogRecord};
use allstak::transport::Transport;
use common::harness;

#[test]
fn hub_diagnostics_are_counter_only_and_include_scope_state() {
    let h = harness(|_| {});

    h.hub.configure_scope(|scope| {
        scope.set_trace_id(Some("0af7651916cd43dd8448eb211c80319c".into()));
        scope.set_span_id(Some("b7ad6b7169203331".into()));
    });
    h.hub.add_breadcrumb(Breadcrumb::new("ready"));
    h.hub
        .capture_event(ErrorEvent::new("E", "user test@example.com"));

    let diagnostics = h.hub.get_diagnostics();

    assert!(diagnostics.events_captured >= 1);
    assert_eq!(diagnostics.events_sent, 1);
    assert_eq!(diagnostics.events_dropped, 0);
    assert_eq!(diagnostics.active_trace_count, 1);
    assert_eq!(diagnostics.active_span_count, 1);
    assert_eq!(diagnostics.breadcrumb_count, 1);
    assert!(diagnostics.sanitizer_redaction_count >= 1);
    assert!(!diagnostics.disabled);
}

#[test]
fn diagnostics_count_before_send_and_before_breadcrumb_drops() {
    let h = harness(|opts| {
        opts.before_send = Some(Arc::new(|_| None));
        opts.before_breadcrumb = Some(Arc::new(|_| None));
    });

    h.hub.add_breadcrumb(Breadcrumb::new("drop me"));
    h.hub.capture_event(ErrorEvent::new("E", "drop me"));

    let diagnostics = h.hub.get_diagnostics();
    assert_eq!(diagnostics.events_captured, 1);
    assert_eq!(diagnostics.events_sent, 0);
    assert_eq!(diagnostics.events_dropped, 2);
    assert_eq!(diagnostics.breadcrumb_count, 0);
}

#[test]
fn stub_transport_diagnostics_count_sent_envelopes() {
    let h = harness(|_| {});

    h.client.capture_log(LogRecord {
        level: "info".into(),
        message: "hello".into(),
        service: None,
        trace_id: None,
        span_id: None,
        request_id: None,
        user_id: None,
        error_id: None,
        metadata: None,
        environment: None,
    });

    let transport_diagnostics = h.transport.diagnostics();
    assert_eq!(transport_diagnostics.events_captured, 1);
    assert_eq!(transport_diagnostics.events_sent, 1);
    assert_eq!(transport_diagnostics.events_dropped, 0);
}
