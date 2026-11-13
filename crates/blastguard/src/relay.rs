//! The subagent Ask relay: a subagent's Ask is adjudicated in the main session.
//!
//! User rulings (2026-11-14, verbatim):
//!
//! * 「sub agentのAskはmainで裁定する」 — the governing statement. A subagent's
//!   Ask is decided in the main session. The subagent never decides it, and no
//!   gate decides it differently.
//! * 「この仕組みはコードで強制する、どうせわすれる」 — the procedure is enforced by
//!   code, because the main agent forgets it.
//! * 「gateにそんな例外は追加してほしくない」 — no exception, bypass, flag or
//!   relay-execution path is added to any verdict.
//! * 「gateもconduktも絶対に勝手に他のworktreeを参照してはならない」 — nothing here
//!   reads, stats or runs anything in the subagent's `cwd`. That `cwd` is stored
//!   and compared as an opaque string only.
//!
//! # The procedure this module enforces
//!
//! 1. Inside a subagent nobody can answer a permission prompt, so `emit`
//!    (main.rs) hardens an `Ask` to a `Deny`. That verdict is unchanged. When
//!    the call was a Bash command and the main session itself could ask
//!    ([`crate::interactive::ask_available`]), the hardened Ask is also
//!    appended to this session's relay queue ([`queue`]).
//! 2. The main agent re-runs the EXACT command (same `cwd`, same string)
//!    through its own Bash tool. blastguard judges that call normally, so the
//!    human sees the real permission prompt. A PostToolUse /
//!    PostToolUseFailure hook ([`mark_executed`]) marks the entry executed.
//! 3. The main agent sends the output and exit code to the subagent with
//!    SendMessage, naming the relay id. A PostToolUse hook on SendMessage
//!    ([`mark_relayed`]) marks the entry relayed.
//! 4. A main-session Stop hook ([`stop_outcome`]) blocks while any entry of
//!    the session is neither (executed and relayed) nor dismissed.
//!
//! The human may decline the prompt; then no PostToolUse fires. That is what
//! [`dismiss`] is for: it requires a non-empty reason, and every dismissal of
//! the session is shown by the Stop hook from then on, so a dismissal is
//! never silent.
//!
//! # What this module never does
//!
//! * It never executes a command. A tool that ran the stored command inside a
//!   single Bash call would hide that command from blastguard (a fail-open);
//!   the only way an entry becomes `executed` is the main agent's own Bash
//!   call, which blastguard's PreToolUse judged like any other.
//! * It never changes a verdict. The queue write happens after the verdict is
//!   decided; a failed write only adds a line to the deny reason.
//! * It never records a true `Deny` (system harm or otherwise). Only an `Ask`
//!   that was hardened because the caller was a subagent is queued.
//!
//! # Fail-closed (CLAUDE.md §3)
//!
//! An unreadable queue, a line that does not parse, or an event naming an id
//! with no `queued` line makes the Stop hook BLOCK and name the file. An absent
//! queue file is a determined "nothing was queued" and allows.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Prefix of every relay id. Short and distinctive so a SendMessage that
/// names the id can be recognised by substring.
pub const ID_PREFIX: &str = "bgr-";

/// Env override of the queue directory. Tests point it at a scratch dir; an
/// operator may point it elsewhere. Empty or whitespace is ignored.
pub const DIR_VAR: &str = "BLASTGUARD_RELAY_DIR";

/// One line of a session's queue file. Append-only: state is the fold of all
/// lines, so concurrent writers (parallel subagents) only ever append.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "lowercase")]
pub enum Event {
    /// A subagent's Ask that was hardened to a Deny.
    Queued {
        id: String,
        ts: u64,
        session_id: String,
        agent_id: Option<String>,
        cwd: String,
        command: String,
        reason: String,
    },
    /// The main session ran the exact command through its own Bash tool.
    Executed {
        id: String,
        ts: u64,
        exit_code: Option<i64>,
    },
    /// The main session sent a SendMessage naming the id, after executing.
    Relayed { id: String, ts: u64 },
    /// A human-supplied reason why the entry will not be re-run.
    Dismissed { id: String, ts: u64, reason: String },
}

/// The folded state of one queued entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    pub ts: u64,
    pub agent_id: Option<String>,
    pub cwd: String,
    pub command: String,
    pub reason: String,
    /// `Some(exit_code)` once executed (the exit code itself may be unknown).
    pub executed: Option<Option<i64>>,
    pub relayed: bool,
    pub dismissed: Option<String>,
}

impl Entry {
    /// Resolved: executed AND relayed, or dismissed with a reason.
    pub fn is_resolved(&self) -> bool {
        self.dismissed.is_some() || (self.executed.is_some() && self.relayed)
    }
}

/// The queue directory: `BLASTGUARD_RELAY_DIR`, else `~/.blastguard/relay`.
/// Lives in blastguard's own state dir, outside every repo and worktree.
pub fn relay_dir() -> PathBuf {
    if let Ok(d) = std::env::var(DIR_VAR) {
        if !d.trim().is_empty() {
            return PathBuf::from(d.trim());
        }
    }
    harness_core::config::base_dir("blastguard").join("relay")
}

/// The session key: the payload's `session_id`, else `CLAUDE_CODE_SESSION_ID`,
/// else `_local`. A subagent's payload carries its parent's session id (its
/// transcript records the parent's `sessionId`), which is what makes the
/// subagent's record visible to the main session's Stop.
pub fn session_of(payload_session: &str) -> String {
    let s = payload_session.trim();
    if !s.is_empty() {
        return s.to_string();
    }
    match std::env::var("CLAUDE_CODE_SESSION_ID") {
        Ok(v) if !v.trim().is_empty() => v.trim().to_string(),
        _ => "_local".to_string(),
    }
}

/// `<dir>/<session>.jsonl`. A session id that is not a plain
/// `[A-Za-z0-9_-]` token is hashed so it can never name a path outside `dir`.
pub fn queue_path(dir: &Path, session: &str) -> PathBuf {
    let plain = !session.is_empty()
        && session
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    let name = if plain {
        session.to_string()
    } else {
        format!("h-{}", &sha_hex(session.as_bytes())[..32])
    };
    dir.join(format!("{name}.jsonl"))
}

fn sha_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(bytes))
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn new_id(session: &str, cwd: &str, command: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seed = format!(
        "{session}\u{1}{cwd}\u{1}{command}\u{1}{nanos}\u{1}{}",
        std::process::id()
    );
    format!("{ID_PREFIX}{}", &sha_hex(seed.as_bytes())[..12])
}

fn append(path: &Path, event: &Event) -> Result<(), String> {
    let line = serde_json::to_string(event).map_err(|e| format!("serialize failed: {e}"))?;
    harness_core::append::append_line(path, &line)
        .map_err(|e| format!("append to {} failed: {e}", path.display()))
}

/// Append a `queued` record and return its id, or the reason it could not be
/// written (which the caller surfaces in the deny reason).
pub fn queue(
    dir: &Path,
    session: &str,
    agent_id: Option<&str>,
    cwd: &str,
    command: &str,
    reason: &str,
) -> Result<String, String> {
    let id = new_id(session, cwd, command);
    let path = queue_path(dir, session);
    append(
        &path,
        &Event::Queued {
            id: id.clone(),
            ts: now(),
            session_id: session.to_string(),
            agent_id: agent_id.map(str::to_string),
            cwd: cwd.to_string(),
            command: command.to_string(),
            reason: reason.to_string(),
        },
    )?;
    Ok(id)
}

/// The queue of one session as read from disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Queue {
    /// No file: nothing was ever queued for this session.
    Absent,
    /// Every line parsed and every event refers to a queued id.
    Known(Vec<Entry>),
}

/// Read and fold a queue file. `Err` (with the path in the message) for any
/// IO error other than not-found, an unparseable line, or an event that names
/// an id without a `queued` line — all of which the Stop hook blocks on.
pub fn load(path: &Path) -> Result<Queue, String> {
    use harness_core::verdict::Determination;
    let text = match harness_core::boundary::read_to_string(path) {
        Determination::Known(Some(t)) => t,
        Determination::Known(None) => return Ok(Queue::Absent),
        Determination::Undetermined(why) => {
            return Err(format!(
                "cannot read relay queue {}: {}",
                path.display(),
                why.as_str()
            ))
        }
    };
    let mut entries: Vec<Entry> = Vec::new();
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let event: Event = serde_json::from_str(line).map_err(|e| {
            format!(
                "relay queue {} line {} does not parse ({e})",
                path.display(),
                n + 1
            )
        })?;
        let unknown = |id: &str| {
            format!(
                "relay queue {} line {} names id {id} that has no queued record",
                path.display(),
                n + 1
            )
        };
        match event {
            Event::Queued {
                id,
                ts,
                agent_id,
                cwd,
                command,
                reason,
                ..
            } => entries.push(Entry {
                id,
                ts,
                agent_id,
                cwd,
                command,
                reason,
                executed: None,
                relayed: false,
                dismissed: None,
            }),
            Event::Executed { id, exit_code, .. } => {
                let e = entries
                    .iter_mut()
                    .find(|e| e.id == id)
                    .ok_or_else(|| unknown(&id))?;
                e.executed = Some(exit_code);
            }
            Event::Relayed { id, .. } => {
                let e = entries
                    .iter_mut()
                    .find(|e| e.id == id)
                    .ok_or_else(|| unknown(&id))?;
                e.relayed = true;
            }
            Event::Dismissed { id, reason, .. } => {
                let e = entries
                    .iter_mut()
                    .find(|e| e.id == id)
                    .ok_or_else(|| unknown(&id))?;
                e.dismissed = Some(reason);
            }
        }
    }
    Ok(Queue::Known(entries))
}

fn known_entries(path: &Path) -> Result<Vec<Entry>, String> {
    match load(path)? {
        Queue::Absent => Ok(Vec::new()),
        Queue::Known(v) => Ok(v),
    }
}

/// PostToolUse(Failure) on Bash in the MAIN session: mark the oldest
/// unexecuted, undismissed entry whose `command` and `cwd` are byte-identical
/// to this call's. Returns the id marked, if any. The `cwd` is compared as a
/// string and never resolved.
pub fn mark_executed(
    dir: &Path,
    session: &str,
    cwd: &str,
    command: &str,
    exit_code: Option<i64>,
) -> Result<Option<String>, String> {
    let path = queue_path(dir, session);
    let entries = known_entries(&path)?;
    let Some(hit) = entries.iter().find(|e| {
        e.executed.is_none() && e.dismissed.is_none() && e.command == command && e.cwd == cwd
    }) else {
        return Ok(None);
    };
    append(
        &path,
        &Event::Executed {
            id: hit.id.clone(),
            ts: now(),
            exit_code,
        },
    )?;
    Ok(Some(hit.id.clone()))
}

/// PostToolUse on SendMessage in the MAIN session: mark every executed,
/// not-yet-relayed entry whose id occurs in `message_text`. An entry that was
/// not executed yet is NOT marked: sending before running relays nothing.
pub fn mark_relayed(dir: &Path, session: &str, message_text: &str) -> Result<Vec<String>, String> {
    let path = queue_path(dir, session);
    let entries = known_entries(&path)?;
    let mut marked = Vec::new();
    for e in entries.iter().filter(|e| {
        e.executed.is_some()
            && !e.relayed
            && e.dismissed.is_none()
            && message_text.contains(e.id.as_str())
    }) {
        append(
            &path,
            &Event::Relayed {
                id: e.id.clone(),
                ts: now(),
            },
        )?;
        marked.push(e.id.clone());
    }
    Ok(marked)
}

/// Why a dismissal was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum DismissError {
    /// No reason, or a blank one. Exit 2.
    EmptyReason,
    /// The id is not in any readable queue file. Exit 2.
    UnknownId(String),
    /// A queue could not be read or written. Exit 3.
    Io(String),
}

/// `blastguard relay dismiss <id> --reason <text>`: find the id in the queue
/// directory and append a `dismissed` record carrying the reason. Returns the
/// queue file it was written to.
///
/// If the id is not found and some queue file could not be read, that is
/// reported as `Io` (the id may be in the unreadable file), not `UnknownId`.
pub fn dismiss(dir: &Path, id: &str, reason: &str) -> Result<PathBuf, DismissError> {
    if reason.trim().is_empty() {
        return Err(DismissError::EmptyReason);
    }
    use harness_core::verdict::Determination;
    // All-or-nothing: a listing error mid-walk is `Undetermined` for the whole
    // directory, never a partial listing.
    let listing = match harness_core::boundary::read_dir_entries_checked(dir) {
        Determination::Known(Some(l)) => l,
        Determination::Known(None) => return Err(DismissError::UnknownId(id.to_string())),
        Determination::Undetermined(why) => {
            return Err(DismissError::Io(format!(
                "cannot list relay dir {}: {}",
                dir.display(),
                why.as_str()
            )))
        }
    };
    let mut unreadable: Vec<String> = Vec::new();
    for path in listing {
        if path.extension().and_then(|x| x.to_str()) != Some("jsonl") {
            continue;
        }
        match known_entries(&path) {
            Ok(entries) => {
                if entries.iter().any(|e| e.id == id) {
                    append(
                        &path,
                        &Event::Dismissed {
                            id: id.to_string(),
                            ts: now(),
                            reason: reason.trim().to_string(),
                        },
                    )
                    .map_err(DismissError::Io)?;
                    return Ok(path);
                }
            }
            Err(e) => unreadable.push(e),
        }
    }
    if unreadable.is_empty() {
        Err(DismissError::UnknownId(id.to_string()))
    } else {
        Err(DismissError::Io(unreadable.join("; ")))
    }
}

/// What the Stop hook does.
#[derive(Debug, PartialEq, Eq)]
pub enum StopOutcome {
    /// Block with this reason.
    Block(String),
    /// Allow; `Some(msg)` is a `systemMessage` listing this session's
    /// dismissals so they stay visible.
    Allow(Option<String>),
}

/// The procedure, stated once for every block message.
const PROCEDURE: &str = "User ruling 「sub agentのAskはmainで裁定する」: a subagent's Ask is adjudicated \
in the main session. Inside the subagent blastguard had to deny it (nobody can answer a permission \
prompt there). For each pending entry below:\n\
  1. Run the EXACT command, byte-for-byte, with your own Bash tool, in the listed cwd (if your shell \
is elsewhere, `cd` there in a separate Bash call first). blastguard judges it normally, so the human \
sees the real permission prompt. Do not rewrite it, wrap it, or hand it to the user to run.\n\
  2. SendMessage the output and exit code to the subagent, and name the relay id in the message.\n\
If the human declined the prompt, or the command cannot be run, record why with \
`blastguard relay dismiss <id> --reason \"<why>\"`; every dismissal is shown on each later Stop of \
this session.";

fn describe(e: &Entry) -> String {
    let state = match (&e.executed, e.relayed) {
        (None, _) => "not executed in the main session".to_string(),
        (Some(code), false) => format!(
            "executed (exit code {}), not yet relayed by SendMessage",
            code.map_or("unknown".to_string(), |c| c.to_string())
        ),
        (Some(_), true) => "executed and relayed".to_string(),
    };
    format!(
        "- relay id: {}\n  agent_id: {}\n  cwd: {}\n  state: {state}\n  command:\n{}\n",
        e.id,
        e.agent_id.as_deref().unwrap_or("unknown"),
        e.cwd,
        indent(&e.command)
    )
}

fn indent(s: &str) -> String {
    s.lines()
        .map(|l| format!("    {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn describe_dismissed(e: &Entry) -> String {
    format!(
        "- relay id: {} (cwd: {}) dismissed — reason: {}\n  command:\n{}\n",
        e.id,
        e.cwd,
        e.dismissed.as_deref().unwrap_or(""),
        indent(&e.command)
    )
}

/// The Stop verdict for `session`. Blocks on any unresolved entry and on any
/// queue that cannot be read or folded (fail-closed, path named). Blocking
/// does not depend on `stop_hook_active`: the way out is to resolve or
/// dismiss, never to stop twice.
pub fn stop_outcome(dir: &Path, session: &str) -> StopOutcome {
    let path = queue_path(dir, session);
    let entries = match load(&path) {
        Ok(Queue::Absent) => return StopOutcome::Allow(None),
        Ok(Queue::Known(v)) => v,
        Err(why) => {
            return StopOutcome::Block(format!(
                "blastguard relay: the subagent Ask relay queue for this session could not be \
                 determined, so this stop is blocked (fail-closed): {why}\nqueue file: {}\n\
                 Inspect or repair that file; a subagent Ask recorded there may still be waiting \
                 for the main session.",
                path.display()
            ))
        }
    };
    let pending: Vec<&Entry> = entries.iter().filter(|e| !e.is_resolved()).collect();
    let dismissed: Vec<&Entry> = entries.iter().filter(|e| e.dismissed.is_some()).collect();
    let dismissed_text = if dismissed.is_empty() {
        None
    } else {
        Some(format!(
            "blastguard relay: {} subagent Ask(s) were dismissed in this session instead of being \
             re-run in the main session:\n{}",
            dismissed.len(),
            dismissed
                .iter()
                .map(|e| describe_dismissed(e))
                .collect::<String>()
        ))
    };
    if pending.is_empty() {
        return StopOutcome::Allow(dismissed_text);
    }
    let mut reason = format!(
        "blastguard relay: {} subagent Ask(s) in this session have not been adjudicated in the \
         main session.\n{PROCEDURE}\nqueue file: {}\n\nPending:\n{}",
        pending.len(),
        path.display(),
        pending.iter().map(|e| describe(e)).collect::<String>()
    );
    if let Some(d) = dismissed_text {
        reason.push('\n');
        reason.push_str(&d);
    }
    StopOutcome::Block(reason)
}

/// The exit code of a Bash call as reported in a PostToolUse(Failure)
/// payload, when it is there: an integer `exit_code` / `exitCode` /
/// `returnCode` / `code` in `tool_response`, else `Exit code N` in a failure's
/// `error` text. `None` means "not reported", recorded as unknown.
pub fn exit_code_of(payload: &serde_json::Value) -> Option<i64> {
    if let Some(resp) = payload.get("tool_response") {
        for k in ["exit_code", "exitCode", "returnCode", "code"] {
            if let Some(c) = resp.get(k).and_then(serde_json::Value::as_i64) {
                return Some(c);
            }
        }
    }
    let err = payload.get("error").and_then(serde_json::Value::as_str)?;
    let rest = &err[err.find("Exit code ")? + "Exit code ".len()..];
    let digits: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '-')
        .collect();
    digits.parse().ok()
}

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("bg-relay-unit-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn non_token_session_ids_cannot_escape_the_dir() {
        let d = Path::new("/r");
        assert_eq!(queue_path(d, "abc-1_2"), PathBuf::from("/r/abc-1_2.jsonl"));
        let p = queue_path(d, "../../etc/x");
        assert_eq!(p.parent(), Some(d));
    }

    #[test]
    fn exit_code_from_response_or_failure_text() {
        let v = serde_json::json!({"tool_response": {"exit_code": 3}});
        assert_eq!(exit_code_of(&v), Some(3));
        let v = serde_json::json!({"error": "Exit code 127\nnot found"});
        assert_eq!(exit_code_of(&v), Some(127));
        let v = serde_json::json!({"tool_response": {"stdout": ""}});
        assert_eq!(exit_code_of(&v), None);
    }

    #[test]
    fn lifecycle_folds_to_resolved() {
        let d = tmp("life");
        let id = queue(&d, "s", Some("a1"), "/w", "echo hi", "r").unwrap();
        assert!(matches!(stop_outcome(&d, "s"), StopOutcome::Block(_)));
        assert_eq!(mark_relayed(&d, "s", &id).unwrap(), Vec::<String>::new());
        assert_eq!(
            mark_executed(&d, "s", "/w", "echo hi", Some(0)).unwrap(),
            Some(id.clone())
        );
        assert!(matches!(stop_outcome(&d, "s"), StopOutcome::Block(_)));
        assert_eq!(mark_relayed(&d, "s", &id).unwrap(), vec![id.clone()]);
        assert_eq!(stop_outcome(&d, "s"), StopOutcome::Allow(None));
    }

    #[test]
    fn event_for_unknown_id_is_corrupt() {
        let d = tmp("unknown");
        let p = queue_path(&d, "s");
        append(
            &p,
            &Event::Relayed {
                id: "bgr-x".into(),
                ts: 0,
            },
        )
        .unwrap();
        assert!(load(&p).is_err());
        assert!(matches!(stop_outcome(&d, "s"), StopOutcome::Block(_)));
    }

    #[test]
    fn dismiss_needs_a_reason() {
        let d = tmp("dismiss");
        let id = queue(&d, "s", None, "/w", "x", "r").unwrap();
        assert_eq!(dismiss(&d, &id, "  "), Err(DismissError::EmptyReason));
        assert!(dismiss(&d, &id, "declined").is_ok());
        match stop_outcome(&d, "s") {
            StopOutcome::Allow(Some(msg)) => assert!(msg.contains(&id) && msg.contains("declined")),
            other => panic!("{other:?}"),
        }
    }
}
