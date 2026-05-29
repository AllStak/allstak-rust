//! Spans / transactions must serialize to the contract shape on /spans.

mod common;

use allstak::envelope::DataCategory;
use allstak::Hub;
use common::harness;

#[test]
fn transaction_and_child_share_trace_id() {
    let h = harness(|opts| opts.server_name = Some("svc".into()));

    Hub::run(h.hub.clone(), || {
        let tx = allstak::start_transaction("http.server", "GET /things");
        let trace_id = tx.trace_id().to_string();
        let parent_id = tx.span_id().to_string();

        let mut child = tx.start_child("db.query", "SELECT 1");
        child.set_status("ok");
        child.set_tag("db.system", "postgres");
        let child_trace = child.trace_id().to_string();
        child.finish();
        tx.finish();

        assert_eq!(trace_id, child_trace);
        assert!(!parent_id.is_empty());
    });

    let spans = h.transport.sent_for(DataCategory::Transaction);
    assert_eq!(spans.len(), 2);

    // First finished is the child; verify contract fields.
    let child = &spans[0].body["spans"][0];
    assert_eq!(spans[0].path, "/ingest/v1/spans");
    assert_eq!(child["operation"], "db.query");
    assert_eq!(child["description"], "SELECT 1");
    assert_eq!(child["status"], "ok");
    assert_eq!(child["tags"]["db.system"], "postgres");
    assert_eq!(child["service"], "svc");
    assert_eq!(child["environment"], "test");
    assert!(child["traceId"].is_string());
    assert!(child["spanId"].is_string());
    assert!(child["startTimeMillis"].is_number());
    assert!(child["endTimeMillis"].is_number());
    assert!(child["durationMs"].is_number());

    let root = &spans[1].body["spans"][0];
    assert_eq!(root["operation"], "http.server");
    assert!(root.get("parentSpanId").is_none());
}
