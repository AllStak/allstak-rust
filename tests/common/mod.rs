//! Shared test helpers: build a client wired to an in-memory stub transport so
//! tests can assert on payload shape without any network.
//!
//! Not every test binary uses every helper, so dead-code is allowed here.
#![allow(dead_code)]

use std::sync::Arc;

use allstak::options::ClientOptions;
use allstak::transport::{StubTransport, StubTransportFactory};
use allstak::{Client, Hub, Scope};

/// A client plus the stub transport it sends through.
pub struct TestHarness {
    pub client: Arc<Client>,
    pub transport: StubTransport,
    pub hub: Arc<Hub>,
}

/// Build a harness with sane defaults, applying `tweak` to the options first.
pub fn harness(tweak: impl FnOnce(&mut ClientOptions)) -> TestHarness {
    let transport = StubTransport::new();
    let mut opts = ClientOptions {
        api_key: "test-key".to_string(),
        release: Some("test@1.0.0".to_string()),
        environment: Some("test".to_string()),
        ..ClientOptions::default()
    };
    // Don't auto-install integrations / panic hooks in unit tests.
    opts.default_integrations = false;
    opts.auto_session_tracking = false;
    opts.transport = Some(Arc::new(StubTransportFactory::new(transport.clone())));
    tweak(&mut opts);

    let client = Client::new(opts);
    let hub = Arc::new(Hub::new(Some(client.clone()), Scope::new(100)));
    TestHarness {
        client,
        transport,
        hub,
    }
}

/// Parse the single error envelope body, panicking if there isn't exactly one.
pub fn single_error_body(transport: &StubTransport) -> serde_json::Value {
    let envs = transport.sent_for(allstak::envelope::DataCategory::Error);
    assert_eq!(envs.len(), 1, "expected exactly one error envelope");
    assert_eq!(envs[0].path, "/ingest/v1/errors");
    envs[0].body.clone()
}
