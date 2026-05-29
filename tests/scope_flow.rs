//! Scope user / tags / breadcrumbs must flow into captured events.

mod common;

use allstak::protocol::{Breadcrumb, ErrorEvent, User};
use common::{harness, single_error_body};

#[test]
fn scope_user_and_tags_flow_into_event() {
    let h = harness(|_| {});

    h.hub.configure_scope(|scope| {
        scope.set_user(Some(User {
            id: Some("user-42".into()),
            email: None,
            ip: None,
        }));
        scope.set_tag("feature", "checkout");
        scope.set_extra("cart_size", serde_json::json!(3));
    });

    h.hub.capture_event(ErrorEvent::new("E", "m"));
    let body = single_error_body(&h.transport);

    assert_eq!(body["user"]["id"], "user-42");
    assert_eq!(body["metadata"]["tags"]["feature"], "checkout");
    assert_eq!(body["metadata"]["extra"]["cart_size"], 3);
}

#[test]
fn breadcrumbs_flow_into_event_and_trim_to_cap() {
    let h = harness(|opts| opts.max_breadcrumbs = 3);

    // Re-create the hub scope with the configured cap.
    h.hub.configure_scope(|scope| {
        *scope = allstak::Scope::new(3);
    });

    for i in 0..10 {
        h.hub.add_breadcrumb(Breadcrumb::new(format!("step {i}")));
    }
    h.hub.capture_event(ErrorEvent::new("E", "m"));

    let body = single_error_body(&h.transport);
    let crumbs = body["breadcrumbs"].as_array().expect("breadcrumbs array");
    // Ring buffer trimmed to the last 3.
    assert_eq!(crumbs.len(), 3);
    assert_eq!(crumbs[0]["message"], "step 7");
    assert_eq!(crumbs[2]["message"], "step 9");
}

#[test]
fn with_scope_is_isolated() {
    let h = harness(|_| {});

    h.hub.with_scope(
        |scope| scope.set_tag("temp", "yes"),
        || {
            h.hub.capture_event(ErrorEvent::new("E", "inside"));
        },
    );

    // After the scope pops, a new event must not carry the temp tag.
    h.hub.capture_event(ErrorEvent::new("E", "outside"));

    let envs = h.transport.sent_for(allstak::envelope::DataCategory::Error);
    assert_eq!(envs.len(), 2);
    assert_eq!(envs[0].body["metadata"]["tags"]["temp"], "yes");
    assert!(envs[1].body.get("metadata").is_none());
}
