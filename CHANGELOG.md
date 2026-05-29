# Changelog

All notable changes to the `allstak` crate are documented here. This project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-05-29

Initial release. A native Rust SDK implementing the AllStak ingest wire
protocol directly.

### Added

- **Core API**: `init` returning a `ClientInitGuard` that flushes on drop with
  a configurable `shutdown_timeout`. Accepts a bare api key, a
  `(api_key, ClientOptions)` tuple, or a full `ClientOptions`. Env fallback to
  `ALLSTAK_API_KEY` / `ALLSTAK_DSN`.
- **Client / Hub / Scope**: thread-local current hub, `Hub::main`,
  `Hub::new_from_top`, `Hub::run`, `bind_client`, scope stack with
  `push_scope` / `configure_scope` / `with_scope` / `clear`, and scope setters
  for user / tag / extra / context / level / fingerprint / transaction /
  breadcrumb.
- **Capture**: `capture_event`, `capture_error(&dyn Error)`,
  `capture_message`, `event_from_error`, `last_event_id`, and (behind the
  `anyhow` feature) `capture_anyhow` that walks the error chain.
- **Breadcrumbs**: ring buffer trimmed to `max_breadcrumbs` (default 100) with
  a `before_breadcrumb` filter.
- **Event pipeline**: scope application → ordered integrations → `before_send`
  → `sample_rate` → transport.
- **Panic integration** (default-on `panic` feature): global hook that chains
  the prior hook, emits a fatal-level event with a backtrace, marks the active
  session crashed, supports custom extractors, and flushes before exit.
- **Backtrace capture** with `in_app` marking from `in_app_include` /
  `in_app_exclude`.
- **Release-health sessions**: `SessionMode` (`Application` / `Request`),
  `auto_session_tracking`, `start_session` / `end_session` /
  `end_session_with_status`, and `SessionStatus`.
- **Transport**: `Transport` trait (`send_envelope` / `flush` / `shutdown`) +
  `TransportFactory`; default async `reqwest` transport on a background worker
  thread with a bounded queue, retry/backoff on transient failures, and
  per-category rate-limit handling (`429` + `Retry-After`). In-memory stub
  transport for testing.
- **Privacy-by-default PII scrubbing**: value-pattern redaction of email,
  national-insurance/SSN, and payment-card patterns, gated by
  `send_default_pii` (default `false`).
- **Performance tracing**: `start_transaction` / `start_span` and child spans
  posted to `/ingest/v1/spans`, with distributed-trace continuation.
- **Trace propagation**: reads `X-AllStak-Trace-Id` / `X-Trace-Id`,
  `X-Request-Id` / `X-AllStak-Request-Id`, and W3C `traceparent`.
- **`tracing` feature**: a `tracing-subscriber` layer mapping events to
  logs/breadcrumbs/events (configurable `EventFilter`) and spans to spans, with
  `tags.`-prefixed field extraction.
- **`axum` feature**: a tower `Layer` that binds a per-request hub, records the
  request with the matched route-template path, opens a request span, and
  captures `5xx` responses.
- **`actix` feature**: actix-web middleware with a per-request hub, request
  recording, trace continuation, and server-error capture.
- Wire payloads serialize with serde to the exact camelCase field names of the
  AllStak ingest contract. Requests carry `X-AllStak-Key` and a
  `allstak-rust/<version>` User-Agent.

[0.1.0]: https://github.com/AllStak/allstak-rust/releases/tag/v0.1.0
