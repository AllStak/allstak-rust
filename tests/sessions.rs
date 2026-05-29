//! Sessions must start on the hub and end with the correct status, and an
//! error-level event must mark the active session errored.

mod common;

use allstak::envelope::DataCategory;
use allstak::protocol::{ErrorEvent, Level, SessionStatus};
use common::harness;

#[test]
fn session_starts_and_ends_ok() {
    let h = harness(|_| {});
    h.hub.start_session();
    h.hub.end_session();

    let sessions = h.transport.sent_for(DataCategory::Session);
    assert_eq!(sessions.len(), 2);

    let start = &sessions[0];
    assert_eq!(start.path, "/ingest/v1/sessions/start");
    assert!(start.body["sessionId"].is_string());
    assert_eq!(start.body["release"], "test@1.0.0");
    assert_eq!(start.body["sdkName"], "rust");

    let end = &sessions[1];
    assert_eq!(end.path, "/ingest/v1/sessions/end");
    // No error occurred: an Ok session ends as "exited".
    assert_eq!(end.body["status"], "exited");
    assert!(end.body["durationMs"].is_number());
    // Same session id throughout.
    assert_eq!(start.body["sessionId"], end.body["sessionId"]);
}

#[test]
fn error_event_marks_session_errored() {
    let h = harness(|_| {});
    h.hub.start_session();

    let mut event = ErrorEvent::new("E", "fail");
    event.level = Some(Level::Error.as_str().to_string());
    h.hub.capture_event(event);

    h.hub.end_session();

    let sessions = h.transport.sent_for(DataCategory::Session);
    let end = sessions.last().expect("an end session");
    assert_eq!(end.body["status"], "errored");
}

#[test]
fn explicit_status_is_respected() {
    let h = harness(|_| {});
    h.hub.start_session();
    h.hub.end_session_with_status(SessionStatus::Crashed);

    let sessions = h.transport.sent_for(DataCategory::Session);
    let end = sessions.last().expect("an end session");
    assert_eq!(end.body["status"], "crashed");
}

#[test]
fn captured_error_carries_active_session_id() {
    let h = harness(|_| {});
    h.hub.start_session();
    h.hub.capture_event(ErrorEvent::new("E", "with-session"));

    let sessions = h.transport.sent_for(DataCategory::Session);
    let session_id = sessions[0].body["sessionId"].as_str().unwrap();

    let errors = h.transport.sent_for(DataCategory::Error);
    assert_eq!(errors[0].body["sessionId"], session_id);
}
