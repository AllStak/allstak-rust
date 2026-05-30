//! `init_from_env` is the zero-config entry point: it reads options from the
//! environment, installs the default integrations (incl. the panic hook) and,
//! on the `tracing` feature, the global tracing subscriber — all with no
//! per-call wiring. This test lives in its own binary so its env/global-state
//! mutation does not race other tests.

use allstak::ClientOptions;

#[test]
fn reads_options_from_env_and_enables_client() {
    // SAFETY: single-threaded test binary; no other thread reads env here.
    unsafe {
        std::env::set_var("ALLSTAK_API_KEY", "env-key");
        std::env::set_var("ALLSTAK_RELEASE", "envsvc@2.0.0");
        std::env::set_var("ALLSTAK_ENVIRONMENT", "staging");
        std::env::set_var("ALLSTAK_SERVER_NAME", "env-svc");
        std::env::set_var("ALLSTAK_SAMPLE_RATE", "0.5");
        std::env::set_var("ALLSTAK_SEND_DEFAULT_PII", "true");
        std::env::set_var("ALLSTAK_DEBUG", "1");
    }

    let guard = allstak::init_from_env();
    assert!(
        guard.is_enabled(),
        "a key from the environment yields a live client"
    );

    let client = guard.client();
    let opts = client.options();
    assert_eq!(opts.release.as_deref(), Some("envsvc@2.0.0"));
    assert_eq!(opts.environment.as_deref(), Some("staging"));
    assert_eq!(opts.server_name.as_deref(), Some("env-svc"));
    assert_eq!(opts.sample_rate, 0.5);
    assert!(opts.send_default_pii);
    assert!(opts.debug);

    // Calling again must not panic even though a global subscriber / panic hook
    // is already installed (best-effort, idempotent).
    let guard2 = allstak::init_from_env();
    assert!(guard2.is_enabled());

    // Defaults still apply for unset knobs.
    assert_eq!(opts.host, ClientOptions::default().host);

    unsafe {
        std::env::remove_var("ALLSTAK_API_KEY");
        std::env::remove_var("ALLSTAK_RELEASE");
        std::env::remove_var("ALLSTAK_ENVIRONMENT");
        std::env::remove_var("ALLSTAK_SERVER_NAME");
        std::env::remove_var("ALLSTAK_SAMPLE_RATE");
        std::env::remove_var("ALLSTAK_SEND_DEFAULT_PII");
        std::env::remove_var("ALLSTAK_DEBUG");
    }
    let _ = guard;
    let _ = guard2;
}
