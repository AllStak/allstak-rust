//! The [`Hub`] is the central scope/client manager.
//!
//! Each hub owns a stack of [`Scope`]s, an optional [`Client`] and the active
//! release-health session. A thread-local "current" hub is used for sync code;
//! [`Hub::run`] binds a hub for the duration of a closure.

use std::cell::RefCell;
use std::sync::{Arc, Mutex, RwLock};

use once_cell::sync::Lazy;
use uuid::Uuid;

use crate::client::Client;
use crate::diagnostics::Diagnostics;
use crate::protocol::{Breadcrumb, ErrorEvent, SessionStatus, User};
use crate::scope::Scope;
use crate::session::Session;

/// The process-wide main hub. New threads clone its top scope + client.
static MAIN_HUB: Lazy<Arc<Hub>> = Lazy::new(|| Arc::new(Hub::new(None, Scope::new(100))));

thread_local! {
    static CURRENT_HUB: RefCell<Option<Arc<Hub>>> = const { RefCell::new(None) };
}

/// Last accepted event id, for `last_event_id()`.
static LAST_EVENT_ID: Lazy<Mutex<Option<Uuid>>> = Lazy::new(|| Mutex::new(None));

struct HubInner {
    client: Option<Arc<Client>>,
    scopes: Vec<Scope>,
    session: Option<Session>,
}

/// Central manager binding a [`Client`] to a stack of [`Scope`]s.
pub struct Hub {
    inner: RwLock<HubInner>,
}

/// Pops the top scope off its hub when dropped.
pub struct ScopeGuard {
    hub: Arc<Hub>,
}

impl Drop for ScopeGuard {
    fn drop(&mut self) {
        if let Ok(mut inner) = self.hub.inner.write() {
            if inner.scopes.len() > 1 {
                inner.scopes.pop();
            }
        }
    }
}

impl Hub {
    /// Build a hub with an optional client and a base scope.
    pub fn new(client: Option<Arc<Client>>, scope: Scope) -> Hub {
        Hub {
            inner: RwLock::new(HubInner {
                client,
                scopes: vec![scope],
                session: None,
            }),
        }
    }

    /// Clone `other`'s top scope and share its client into a fresh hub.
    pub fn new_from_top(other: &Hub) -> Arc<Hub> {
        let inner = other.inner.read().expect("hub poisoned");
        let top = inner
            .scopes
            .last()
            .cloned()
            .unwrap_or_else(|| Scope::new(100));
        Arc::new(Hub::new(inner.client.clone(), top))
    }

    /// The process-wide main hub.
    pub fn main() -> Arc<Hub> {
        MAIN_HUB.clone()
    }

    /// The hub active on the current thread (falls back to `main`).
    pub fn current() -> Arc<Hub> {
        CURRENT_HUB
            .with(|c| c.borrow().clone())
            .unwrap_or_else(Hub::main)
    }

    /// Run `f` while `hub` is the current thread-local hub.
    pub fn run<F, R>(hub: Arc<Hub>, f: F) -> R
    where
        F: FnOnce() -> R,
    {
        let prev = CURRENT_HUB.with(|c| c.borrow_mut().replace(hub));
        let result = f();
        CURRENT_HUB.with(|c| {
            *c.borrow_mut() = prev;
        });
        result
    }

    /// Run `f` with the current hub.
    pub fn with<F, R>(f: F) -> R
    where
        F: FnOnce(Arc<Hub>) -> R,
    {
        f(Hub::current())
    }

    /// Bind (or unbind) the client for this hub.
    pub fn bind_client(&self, client: Option<Arc<Client>>) {
        if let Ok(mut inner) = self.inner.write() {
            inner.client = client;
        }
    }

    /// The hub's client, if any.
    pub fn client(&self) -> Option<Arc<Client>> {
        self.inner.read().ok().and_then(|i| i.client.clone())
    }

    /// Whether a live client is bound.
    pub fn is_enabled(&self) -> bool {
        self.client().map(|c| c.is_enabled()).unwrap_or(false)
    }

    /// Mutate the top scope.
    pub fn configure_scope<F>(&self, f: F)
    where
        F: FnOnce(&mut Scope),
    {
        if let Ok(mut inner) = self.inner.write() {
            if let Some(top) = inner.scopes.last_mut() {
                f(top);
            }
        }
    }

    /// Push a temporary scope (initialized from the current top) and run `body`
    /// with it active; the scope is popped afterwards.
    pub fn with_scope<C, F, R>(&self, scope_fn: C, body: F) -> R
    where
        C: FnOnce(&mut Scope),
        F: FnOnce() -> R,
    {
        // Push a clone of the top scope, apply the configurator.
        {
            let mut inner = self.inner.write().expect("hub poisoned");
            let mut top = inner
                .scopes
                .last()
                .cloned()
                .unwrap_or_else(|| Scope::new(100));
            scope_fn(&mut top);
            inner.scopes.push(top);
        }
        let result = body();
        if let Ok(mut inner) = self.inner.write() {
            if inner.scopes.len() > 1 {
                inner.scopes.pop();
            }
        }
        result
    }

    /// Push a clone of the top scope, returning a guard that pops it on drop.
    pub fn push_scope(self: &Arc<Self>) -> ScopeGuard {
        if let Ok(mut inner) = self.inner.write() {
            let top = inner
                .scopes
                .last()
                .cloned()
                .unwrap_or_else(|| Scope::new(100));
            inner.scopes.push(top);
        }
        ScopeGuard { hub: self.clone() }
    }

    /// Add a breadcrumb to the top scope, applying `before_breadcrumb`.
    pub fn add_breadcrumb(&self, breadcrumb: Breadcrumb) {
        let client = self.client();
        let processed = match &client {
            Some(c) => c.process_breadcrumb(breadcrumb),
            None => Some(breadcrumb),
        };
        if let Some(b) = processed {
            self.configure_scope(|scope| scope.add_breadcrumb(b));
        }
    }

    /// Capture an error event, applying the top scope. Marks the active session
    /// errored when the event is at error/fatal level.
    pub fn capture_event(&self, event: ErrorEvent) -> Uuid {
        let client = match self.client() {
            Some(c) => c,
            None => return event.event_id,
        };

        let is_error_level = event
            .level
            .as_deref()
            .map(|l| l == "error" || l == "fatal")
            .unwrap_or(true);

        let event_id = {
            let inner = self.inner.read().expect("hub poisoned");
            let scope = inner
                .scopes
                .last()
                .cloned()
                .unwrap_or_else(|| Scope::new(100));
            // Inject the active session id when present.
            let mut event = event;
            if event.session_id.is_none() {
                if let Some(session) = &inner.session {
                    event.session_id = Some(session.id().to_string());
                }
            }
            client.capture_event(event, &scope)
        };

        if is_error_level {
            if let Ok(mut inner) = self.inner.write() {
                if let Some(session) = inner.session.as_mut() {
                    session.mark_errored();
                }
            }
        }

        if let Ok(mut last) = LAST_EVENT_ID.lock() {
            *last = Some(event_id);
        }
        event_id
    }

    /// Capture a `std::error::Error`.
    pub fn capture_error(&self, error: &dyn std::error::Error) -> Uuid {
        let event = crate::event::event_from_error(error, self.client().as_deref());
        self.capture_event(event)
    }

    /// Capture a plain message at `level`.
    pub fn capture_message(&self, message: &str, level: crate::protocol::Level) -> Uuid {
        let event = crate::event::event_from_message(message, level, self.client().as_deref());
        self.capture_event(event)
    }

    /// Start a release-health session on this hub.
    pub fn start_session(&self) {
        let Some(client) = self.client() else {
            return;
        };
        let mut inner = match self.inner.write() {
            Ok(i) => i,
            Err(_) => return,
        };
        let user = inner.scopes.last().and_then(|s| s.user.clone());
        if let Some(session) = Session::start(client.options(), user.as_ref()) {
            client.send_session_start(&session.to_start());
            inner.session = Some(session);
        }
    }

    /// End the active session with its tracked status.
    pub fn end_session(&self) {
        self.end_session_with_status_internal(None);
    }

    /// End the active session with an explicit status.
    pub fn end_session_with_status(&self, status: SessionStatus) {
        self.end_session_with_status_internal(Some(status));
    }

    fn end_session_with_status_internal(&self, status: Option<SessionStatus>) {
        let Some(client) = self.client() else {
            return;
        };
        let session = {
            let mut inner = match self.inner.write() {
                Ok(i) => i,
                Err(_) => return,
            };
            inner.session.take()
        };
        if let Some(session) = session {
            client.send_session_end(&session.to_end(status));
        }
    }

    /// Mark the active session crashed (used by the panic hook).
    pub fn mark_session_crashed(&self) {
        if let Ok(mut inner) = self.inner.write() {
            if let Some(session) = inner.session.as_mut() {
                session.mark_crashed();
            }
        }
    }

    /// Set the user on the active session as well as the top scope.
    pub fn set_user(&self, user: Option<User>) {
        self.configure_scope(|scope| scope.set_user(user.clone()));
    }

    /// Snapshot the active distributed-trace context from the top scope.
    ///
    /// Used by outbound HTTP and DB instrumentation to propagate the current
    /// trace/span/request ids without the caller threading them by hand.
    pub fn current_trace_context(&self) -> crate::propagation::TraceContext {
        self.inner
            .read()
            .ok()
            .and_then(|i| i.scopes.last().map(|s| s.trace_context()))
            .unwrap_or_default()
    }

    /// Wait for the transport queue to drain.
    pub fn flush(&self, timeout: std::time::Duration) -> bool {
        match self.client() {
            Some(c) => c.flush(timeout),
            None => true,
        }
    }

    /// Counter-only diagnostics for this hub and its client.
    pub fn get_diagnostics(&self) -> Diagnostics {
        let mut diagnostics = self
            .client()
            .map(|client| client.get_diagnostics())
            .unwrap_or_else(|| Diagnostics {
                disabled: true,
                sanitizer_redaction_count: crate::scrub::redaction_count(),
                ..Diagnostics::default()
            });
        if let Ok(inner) = self.inner.read() {
            if let Some(scope) = inner.scopes.last() {
                diagnostics.active_trace_count = if scope.trace_id().is_some() { 1 } else { 0 };
                diagnostics.active_span_count = if scope.span_id().is_some() { 1 } else { 0 };
                diagnostics.breadcrumb_count = scope.breadcrumbs.len() as u64;
            }
        }
        diagnostics
    }
}

/// The last accepted event id on any hub.
pub fn last_event_id() -> Option<Uuid> {
    LAST_EVENT_ID.lock().ok().and_then(|g| *g)
}

/// Bind a client to the main hub (used by `init`).
pub(crate) fn bind_main_client(client: Arc<Client>) {
    Hub::main().bind_client(Some(client));
}
