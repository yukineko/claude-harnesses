//! Per-session state files under `~/.answerlang/state/`:
//!
//! * `<session>.prompt` — language detected from the latest user prompt
//!   (canonical name, or `none`), written by UserPromptSubmit, read by Stop.
//! * `<session>.macos`  — cached macOS UI language (canonical name, or `none`).
//! * `<session>.block`  — "the previous stop of this session was blocked by
//!   answerlang" (bounded re-entry marker).

use std::path::PathBuf;

use harness_core::hook::HookInput;

#[derive(Debug, Clone)]
pub struct Session {
    id: String,
}

/// A session id is used as a file name, so only `[A-Za-z0-9_-]`, 1..=200.
fn usable(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 200
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

impl Session {
    /// The payload's `session_id`, else `CLAUDE_CODE_SESSION_ID`. `None` when
    /// neither is usable (per-session state is then unavailable).
    pub fn resolve(hook: Option<&HookInput>) -> Option<Session> {
        let from_payload = hook.map(|h| h.session_id.trim().to_string());
        let id = match from_payload {
            Some(id) if !id.is_empty() => id,
            _ => std::env::var("CLAUDE_CODE_SESSION_ID")
                .map(|s| s.trim().to_string())
                .unwrap_or_default(),
        };
        if usable(&id) {
            Some(Session { id })
        } else {
            if !id.is_empty() {
                eprintln!("answerlang: session id {id:?} is not usable as a state key");
            }
            None
        }
    }

    fn file(&self, kind: &str) -> PathBuf {
        crate::config::dir()
            .join("state")
            .join(format!("{}.{kind}", self.id))
    }

    /// `Ok(None)` when absent; `Err` when it exists but cannot be read.
    pub fn read(&self, kind: &str) -> Result<Option<String>, String> {
        let p = self.file(kind);
        match std::fs::read_to_string(&p) {
            Ok(s) => Ok(Some(s.trim().to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("state file {} could not be read: {e}", p.display())),
        }
    }

    pub fn write(&self, kind: &str, content: &str) -> Result<(), String> {
        let p = self.file(kind);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
        }
        std::fs::write(&p, format!("{content}\n"))
            .map_err(|e| format!("could not write {}: {e}", p.display()))
    }

    /// Remove; absent is fine.
    pub fn remove(&self, kind: &str) -> Result<(), String> {
        let p = self.file(kind);
        match std::fs::remove_file(&p) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("could not remove {}: {e}", p.display())),
        }
    }
}

pub const PROMPT: &str = "prompt";
pub const MACOS: &str = "macos";
pub const BLOCK: &str = "block";
