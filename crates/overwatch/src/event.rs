/// Event model and lifecycle.
use serde::{Deserialize, Serialize};

/// The kinds of lifecycle events that mark task progression.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EventKind {
    /// Task lease was begun.
    Started,
    /// Task is actively running.
    Running,
    /// Task lease was released with terminal status.
    Ended,
}

/// A single recorded event in the task lifecycle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LifecycleEvent {
    /// What kind of event this is.
    pub kind: EventKind,
    /// The task key (content-key).
    pub key: String,
    /// Human-readable task title.
    pub title: String,
    /// Session ID of the session that owns this event.
    pub session_id: String,
    /// Run ID of the run that owns this event.
    pub run_id: String,
    /// Unix timestamp when the event was recorded.
    pub ts: i64,
    /// Optional status/note (used for Running/Ended events).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

impl LifecycleEvent {
    /// Create a Started event.
    pub fn started(
        key: String,
        title: String,
        session_id: String,
        run_id: String,
        ts: i64,
    ) -> Self {
        Self {
            kind: EventKind::Started,
            key,
            title,
            session_id,
            run_id,
            ts,
            status: None,
        }
    }

    /// Create a Running event with an optional note.
    pub fn running(
        key: String,
        title: String,
        session_id: String,
        run_id: String,
        ts: i64,
        note: Option<String>,
    ) -> Self {
        Self {
            kind: EventKind::Running,
            key,
            title,
            session_id,
            run_id,
            ts,
            status: note,
        }
    }

    /// Create an Ended event with a status.
    pub fn ended(
        key: String,
        title: String,
        session_id: String,
        run_id: String,
        ts: i64,
        status: String,
    ) -> Self {
        Self {
            kind: EventKind::Ended,
            key,
            title,
            session_id,
            run_id,
            ts,
            status: Some(status),
        }
    }
}
