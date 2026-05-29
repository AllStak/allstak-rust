//! Release-health session tracking.

use std::time::Instant;

use crate::options::ClientOptions;
use crate::protocol::{SessionEnd, SessionStart, SessionStatus, User};
use crate::util;

/// A single in-flight release-health session.
#[derive(Debug, Clone)]
pub struct Session {
    session_id: String,
    release: String,
    environment: Option<String>,
    user_id: Option<String>,
    status: SessionStatus,
    started: Instant,
}

impl Session {
    /// Open a fresh session for the given options + optional user.
    pub fn start(options: &ClientOptions, user: Option<&User>) -> Option<Session> {
        // A session needs a release attribute to be meaningful.
        let release = options.release.clone()?;
        Some(Session {
            session_id: util::new_trace_id(),
            release,
            environment: Some(options.resolved_environment()),
            user_id: user.and_then(|u| u.id.clone()),
            status: SessionStatus::Ok,
            started: Instant::now(),
        })
    }

    /// The session id.
    pub fn id(&self) -> &str {
        &self.session_id
    }

    /// Current status.
    pub fn status(&self) -> SessionStatus {
        self.status
    }

    /// Mark the session errored (an error/fatal event occurred). Does not
    /// downgrade a crashed/abnormal status.
    pub fn mark_errored(&mut self) {
        if self.status == SessionStatus::Ok {
            self.status = SessionStatus::Errored;
        }
    }

    /// Mark the session crashed (an unhandled panic occurred).
    pub fn mark_crashed(&mut self) {
        self.status = SessionStatus::Crashed;
    }

    /// Build the `sessions/start` payload.
    pub fn to_start(&self) -> SessionStart {
        SessionStart {
            session_id: self.session_id.clone(),
            release: self.release.clone(),
            environment: self.environment.clone(),
            user_id: self.user_id.clone(),
            sdk_name: Some(util::SDK_NAME.to_string()),
            sdk_version: Some(util::SDK_VERSION.to_string()),
            platform: Some(util::PLATFORM.to_string()),
        }
    }

    /// Build the `sessions/end` payload with a final status. If `status` is
    /// `None` the session's own tracked status is used (defaulting `Ok` to
    /// `Exited`).
    pub fn to_end(&self, status: Option<SessionStatus>) -> SessionEnd {
        let final_status = status.unwrap_or(match self.status {
            SessionStatus::Ok => SessionStatus::Exited,
            other => other,
        });
        SessionEnd {
            session_id: self.session_id.clone(),
            release: self.release.clone(),
            environment: self.environment.clone(),
            user_id: self.user_id.clone(),
            sdk_name: Some(util::SDK_NAME.to_string()),
            sdk_version: Some(util::SDK_VERSION.to_string()),
            platform: Some(util::PLATFORM.to_string()),
            duration_ms: self.started.elapsed().as_millis() as u64,
            status: final_status.as_str().to_string(),
        }
    }
}
