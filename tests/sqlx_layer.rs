//! The sqlx DB layer must turn a `sqlx::query` tracing event into a normalized
//! DbQueryRecord on /ingest/v1/db, tied to the active scope's trace/span — with
//! no per-query code. We emit the exact tracing event shape sqlx produces
//! rather than spin up a real database.

#![cfg(all(feature = "sqlx", feature = "tracing"))]

mod common;

use allstak::envelope::DataCategory;
use allstak::integrations::sqlx::AllstakSqlxLayer;
use allstak::Hub;
use common::harness;
use tracing::Level;
use tracing_subscriber::layer::SubscriberExt;

#[test]
fn sqlx_query_event_becomes_db_record() {
    let h = harness(|opts| opts.server_name = Some("svc".into()));

    let subscriber = tracing_subscriber::registry()
        .with(AllstakSqlxLayer::new().database_type("postgres"));

    Hub::run(h.hub.clone(), || {
        // Seed an active trace/span so the record is correlated.
        h.hub.configure_scope(|s| {
            s.set_trace_id(Some("0af7651916cd43dd8448eb211c80319c".into()));
            s.set_span_id(Some("b7ad6b7169203331".into()));
        });

        tracing::subscriber::with_default(subscriber, || {
            // The exact field set sqlx stamps on its query telemetry event.
            tracing::event!(
                target: "sqlx::query",
                Level::INFO,
                summary = "SELECT users",
                db.statement = "SELECT * FROM users WHERE id = 42",
                rows_affected = 0u64,
                rows_returned = 1u64,
                elapsed_secs = 0.012_f64,
            );
        });
    });

    let dbs = h.transport.sent_for(DataCategory::Db);
    assert_eq!(dbs.len(), 1, "one db query recorded");
    let rec = &dbs[0].body["queries"][0];
    assert_eq!(dbs[0].path, "/ingest/v1/db");
    // Literal stripped from the normalized statement.
    assert_eq!(rec["normalizedQuery"], "SELECT * FROM users WHERE id = ?");
    assert_eq!(rec["queryType"], "SELECT");
    assert_eq!(rec["status"], "ok");
    assert_eq!(rec["databaseType"], "postgres");
    assert_eq!(rec["service"], "svc");
    assert_eq!(rec["environment"], "test");
    assert_eq!(rec["durationMs"], 12);
    // Tied to the active trace/span.
    assert_eq!(rec["traceId"], "0af7651916cd43dd8448eb211c80319c");
    assert_eq!(rec["spanId"], "b7ad6b7169203331");
    assert!(rec["queryHash"].is_string());
}

#[test]
fn non_sqlx_events_are_ignored() {
    let h = harness(|_| {});

    let subscriber = tracing_subscriber::registry().with(AllstakSqlxLayer::new());

    Hub::run(h.hub.clone(), || {
        tracing::subscriber::with_default(subscriber, || {
            tracing::event!(target: "my_app", Level::INFO, "not a query");
        });
    });

    assert!(
        h.transport.sent_for(DataCategory::Db).is_empty(),
        "events off the sqlx::query target produce no db records"
    );
}
