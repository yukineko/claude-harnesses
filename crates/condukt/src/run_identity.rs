//! Run identity recorded by `state init` (moved out of `main.rs`): the terminal
//! label and the owning Claude session.

/// Auto-detect a terminal label: try `tty` output first, fall back to `pid-<PID>`.
pub(crate) fn resolve_label() -> Option<String> {
    let tty = std::process::Command::new("tty").output().ok()?;
    if tty.status.success() {
        let name = String::from_utf8_lossy(&tty.stdout).trim().to_string();
        if !name.is_empty() && name != "not a tty" {
            return Some(name);
        }
    }
    Some(format!("pid-{}", std::process::id()))
}

/// The owning Claude session of a run being initialised: `$CLAUDE_CODE_SESSION_ID`
/// verbatim. Unset, non-UTF-8, empty or whitespace-only is `None` — the owner is
/// undetermined and is recorded as an explicit `null`. Unlike [`resolve_label`]
/// there is deliberately NO fallback: a pid or tty names no session, and a
/// fabricated owner would let an owner filter drop the run as "someone else's".
pub(crate) fn resolve_owner_session() -> Option<String> {
    std::env::var("CLAUDE_CODE_SESSION_ID")
        .ok()
        .filter(|s| !s.trim().is_empty())
}
