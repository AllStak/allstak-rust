# Changelog

All notable changes to the `allstak` crate are documented here. This project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.0] - 2026-05-30

Auto-instrumentation: make outbound HTTP and database tracing automatic, and
add a zero-config entry point. All new behavior is additive and individually
toggleable; existing APIs and payload shapes are unchanged.

### Added

- **`init_from_env`**: zero-config initialization. Reads `ALLSTAK_API_KEY` /
  `ALLSTAK_DSN`, `ALLSTAK_RELEASE`, `ALLSTAK_ENVIRONMENT`,
  `ALLSTAK_SERVER_NAME`, `ALLSTAK_DEBUG`, `ALLSTAK_SAMPLE_RATE`,
  `ALLSTAK_SEND_DEFAULT_PII`, then installs the default integrations (the panic
  hook) and — on the `tracing` feature — the global `tracing` subscriber (the
  log/span layer, plus the `sqlx` DB layer when enabled) best-effort and
  idempotently, so logs, spans and database queries are captured with no
  further wiring.
- **`reqwest-middleware` feature**: `AllstakHttpMiddleware`, a
  `reqwest_middleware::Middleware` that, per outbound request and with no
  per-call code, opens an `http.client` child span under the active trace,
  injects the active trace context (`traceparent` + `X-AllStak-Trace-Id` /
  `X-AllStak-Request-Id`) into the outbound headers, and records an outbound
  `HttpRequestRecord` (`direction: "outbound"`). Crate-root conveniences
  `instrumented_http_client()` / `instrumented_http_client_from(client)` wrap a
  reqwest client in one line.
- **`sqlx` feature**: `AllstakSqlxLayer`, a `tracing` layer that turns sqlx's
  own `sqlx::query` telemetry into normalized `DbQueryRecord`s tied to the
  active span and posted to `/ingest/v1/db` — with no per-query code and no
  sqlx link (works for any sqlx backend). Configurable `database_type` label
  and `min_duration` filter.
- **`propagation::inject`** to complement `extract`: stamps the W3C
  `traceparent` plus `X-AllStak-*` headers from a `TraceContext` (with
  `propagation::format_traceparent`).
- **DB helpers** at the crate root: `normalize_query` (literal/whitespace
  stripping), `query_hash` (stable fingerprint), `query_type` (statement
  classification) and `capture_db_query` (record a query tied to the active
  span), backing the driver integrations and available for manual use.
- **Scope**: `set_span_id` / `set_request_id` (and `span_id` / `trace_id` /
  `request_id` / `trace_context` accessors) plus `Hub::current_trace_context`,
  so outbound HTTP and DB instrumentation nest under the active request/span.
  The `axum` and `actix` middleware now publish the active span/request id so
  outbound calls and DB queries on the same request correlate automatically.

[0.2.0]: https://github.com/AllStak/allstak-rust/releases/tag/v0.2.0

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
