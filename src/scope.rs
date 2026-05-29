//! Per-context data layered onto events.

use std::collections::{BTreeMap, VecDeque};

use serde_json::Value;

use crate::protocol::{Breadcrumb, ErrorEvent, Level, User};

/// Contextual data applied to events captured while it is active.
#[derive(Debug, Clone, Default)]
pub struct Scope {
    pub(crate) user: Option<User>,
    pub(crate) tags: BTreeMap<String, String>,
    pub(crate) extra: BTreeMap<String, Value>,
    pub(crate) contexts: BTreeMap<String, Value>,
    pub(crate) level: Option<Level>,
    pub(crate) fingerprint: Option<Vec<String>>,
    pub(crate) transaction: Option<String>,
    pub(crate) trace_id: Option<String>,
    pub(crate) breadcrumbs: VecDeque<Breadcrumb>,
    pub(crate) max_breadcrumbs: usize,
}

impl Scope {
    /// New empty scope with the given breadcrumb cap.
    pub fn new(max_breadcrumbs: usize) -> Self {
        Scope {
            max_breadcrumbs,
            ..Scope::default()
        }
    }

    /// Set or clear the identified user.
    pub fn set_user(&mut self, user: Option<User>) {
        self.user = user;
    }

    /// Set a string tag.
    pub fn set_tag(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.tags.insert(key.into(), value.into());
    }

    /// Set an extra structured value.
    pub fn set_extra(&mut self, key: impl Into<String>, value: impl Into<Value>) {
        self.extra.insert(key.into(), value.into());
    }

    /// Set a named context object.
    pub fn set_context(&mut self, key: impl Into<String>, value: impl Into<Value>) {
        self.contexts.insert(key.into(), value.into());
    }

    /// Override the level applied to events.
    pub fn set_level(&mut self, level: Option<Level>) {
        self.level = level;
    }

    /// Set or clear the event fingerprint.
    pub fn set_fingerprint(&mut self, fingerprint: Option<&[&str]>) {
        self.fingerprint = fingerprint.map(|f| f.iter().map(|s| s.to_string()).collect());
    }

    /// Set or clear the current transaction name.
    pub fn set_transaction(&mut self, transaction: Option<&str>) {
        self.transaction = transaction.map(|s| s.to_string());
    }

    /// Bind a trace id to this scope (used for distributed tracing).
    pub fn set_trace_id(&mut self, trace_id: Option<String>) {
        self.trace_id = trace_id;
    }

    /// Append a breadcrumb, trimming to the ring-buffer cap.
    pub fn add_breadcrumb(&mut self, breadcrumb: Breadcrumb) {
        self.breadcrumbs.push_back(breadcrumb);
        while self.breadcrumbs.len() > self.max_breadcrumbs {
            self.breadcrumbs.pop_front();
        }
    }

    /// Reset all contextual data (keeps the breadcrumb cap).
    pub fn clear(&mut self) {
        let cap = self.max_breadcrumbs;
        *self = Scope::new(cap);
    }

    /// Apply this scope's data onto an event in place.
    pub(crate) fn apply_to_event(&self, event: &mut ErrorEvent) {
        if event.user.is_none() {
            event.user = self.user.clone();
        }
        if event.trace_id.is_none() {
            event.trace_id = self.trace_id.clone();
        }
        if !self.breadcrumbs.is_empty() {
            let mut crumbs: Vec<Breadcrumb> = self.breadcrumbs.iter().cloned().collect();
            if let Some(existing) = event.breadcrumbs.take() {
                crumbs.extend(existing);
            }
            event.breadcrumbs = Some(crumbs);
        }

        // Merge scope tags/extra/contexts/fingerprint into metadata.
        let mut meta = match event.metadata.take() {
            Some(Value::Object(m)) => m,
            _ => serde_json::Map::new(),
        };
        if !self.tags.is_empty() {
            meta.insert(
                "tags".to_string(),
                serde_json::to_value(&self.tags).unwrap_or(Value::Null),
            );
        }
        if !self.extra.is_empty() {
            meta.insert(
                "extra".to_string(),
                serde_json::to_value(&self.extra).unwrap_or(Value::Null),
            );
        }
        if !self.contexts.is_empty() {
            meta.insert(
                "contexts".to_string(),
                serde_json::to_value(&self.contexts).unwrap_or(Value::Null),
            );
        }
        if let Some(fp) = &self.fingerprint {
            meta.insert(
                "fingerprint".to_string(),
                serde_json::to_value(fp).unwrap_or(Value::Null),
            );
        }
        if let Some(tx) = &self.transaction {
            meta.insert("transaction".to_string(), Value::String(tx.clone()));
        }
        if !meta.is_empty() {
            event.metadata = Some(Value::Object(meta));
        }

        if let Some(level) = self.level {
            if event.level.is_none() {
                event.level = Some(level.as_str().to_string());
            }
        }
    }
}
