//! The block log: a blastguard-owned, durable record of every call it stopped.
//!
//! User ruling (2026-11-14, verbatim):
//! 「…完全なコマンドが残っていないため、これ以上は切り分けられません。というのは大変愚かなので
//! 記録をとること。止めたなら止めた責任をとる」
//!
//! Every printed `Deny` or `Ask` — including an `Ask` hardened to `Deny` in a
//! subagent or a headless session, and a repeat `Deny` downgraded to `Ask` —
//! appends ONE JSON line here that holds exactly what was blocked: the
//! command byte-exact (Bash) or the full `tool_input` (file tools), the cwd,
//! the session and agent ids, the decision and how it was reached, the
//! detector's original verdict (`original_decision`), the reason
//! the agent saw, and the rule id. An `Allow` writes nothing.
//!
//! # Why not (only) the overwatch violation ledger
//!
//! That ledger is overwatch's store, resolved per project, and its schema is
//! built for recurrence signatures, not reproduction: it has no `agent_id`, no
//! file-tool `tool_input`, and no way to say an `Ask` was hardened. This log
//! lives in blastguard's own state dir (`~/.blastguard/blocks/blocks.jsonl`,
//! or `$BLASTGUARD_BLOCK_LOG_DIR/blocks.jsonl`), so a block is recorded
//! wherever blastguard runs, with or without overwatch. The overwatch ledger
//! write is kept unchanged alongside it.
//!
//! # Privacy and size
//!
//! The record stays under the user's home and is never sent anywhere. Nothing
//! is redacted (a redacted command cannot be reproduced). The Bash `command`
//! is never truncated. Only the `tool_input` copy is capped: when its compact
//! serialization exceeds [`TOOL_INPUT_CAP_BYTES`] (a large `Write` content),
//! `tool_input` is `null` and `tool_input_capped` is `true`; the byte length
//! and the sha256 of the full serialization are always recorded, and
//! `file_path` and `command` are kept in full.
//!
//! # A lost record is never silent (CLAUDE.md §1/§3/§4)
//!
//! [`write`] returns the failure. The caller appends [`unrecorded_note`] to the
//! printed reason and warns on stderr, so "nothing was recorded" can never
//! read as "nothing was blocked". The verdict itself is decided before the
//! record is built and is never changed by it.
//!
//! # What this module never does
//!
//! It never decides anything and never inspects `cwd` (it is stored as an
//! opaque string, like the relay's).

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Env override of the log directory. Tests point it at a scratch dir; an
/// operator may point it elsewhere. Empty or whitespace is ignored.
pub const DIR_VAR: &str = "BLASTGUARD_BLOCK_LOG_DIR";

/// File name inside the log directory.
pub const FILE_NAME: &str = "blocks.jsonl";

/// Record schema version. 2 added `original_decision`; a v1 record has no
/// such field, and its absence there means "not recorded", never "deny".
pub const VERSION: u64 = 2;

/// Above this many bytes of compact `tool_input` JSON the copy is dropped
/// (`tool_input: null`, `tool_input_capped: true`). The hash, the length,
/// `command` and `file_path` are kept regardless.
pub const TOOL_INPUT_CAP_BYTES: usize = 1 << 20;

/// The substring every unrecorded-block note carries.
pub const UNRECORDED_MARK: &str = "could NOT be recorded";

/// How the printed decision was reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionKind {
    /// The detector's `Deny`, printed as is.
    Deny,
    /// The detector's `Ask`, printed as is (a human can answer it).
    Ask,
    /// An `Ask` printed as a `Deny` because nobody can answer it (subagent or
    /// headless session).
    AskHardenedToDeny,
    /// A repeat `Deny` handed back as an `Ask` (operator ruling 2026-09-18).
    /// The `Deny` being downgraded may itself be a hardened `Ask` (headless
    /// session): `hardened_ask` keeps which, so the record's
    /// `original_decision` says `ask` for it instead of reading as a repeat of
    /// a real `Deny`.
    DenyDowngradedToAsk { hardened_ask: bool },
    /// The verdict path panicked and the call was blocked with exit 2. The
    /// record keeps the raw stdin in `raw_payload`.
    PanicBlocked,
}

impl DecisionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            DecisionKind::Deny => "deny",
            DecisionKind::Ask => "ask",
            DecisionKind::AskHardenedToDeny => "ask-hardened-to-deny",
            DecisionKind::DenyDowngradedToAsk { .. } => "deny-downgraded-to-ask",
            DecisionKind::PanicBlocked => "panic-blocked",
        }
    }

    /// The detector's own verdict before any hardening or downgrade, as
    /// recorded in `original_decision`. Total over every kind, so the field is
    /// always present and never null: `undetermined` is a panic on the verdict
    /// path (no verdict was reached), never a stand-in for `deny`.
    pub fn original_decision(self) -> &'static str {
        match self {
            DecisionKind::Deny => "deny",
            DecisionKind::Ask => "ask",
            DecisionKind::AskHardenedToDeny => "ask",
            DecisionKind::DenyDowngradedToAsk { hardened_ask: true } => "ask",
            DecisionKind::DenyDowngradedToAsk {
                hardened_ask: false,
            } => "deny",
            DecisionKind::PanicBlocked => "undetermined",
        }
    }
}

/// Everything about one block that is not in the payload.
#[derive(Debug, Clone)]
pub struct Block<'a> {
    /// Final printed decision: `"deny"` or `"ask"`.
    pub decision: &'a str,
    pub kind: DecisionKind,
    /// `"subagent"`, `"interactive"` or `"headless"`.
    pub actor: &'a str,
    pub system_harm: bool,
    /// The reason exactly as printed to the agent.
    pub reason: &'a str,
    pub rule_id: &'a str,
}

/// The log directory: `BLASTGUARD_BLOCK_LOG_DIR`, else `~/.blastguard/blocks`.
pub fn log_dir() -> PathBuf {
    if let Ok(d) = std::env::var(DIR_VAR) {
        if !d.trim().is_empty() {
            return PathBuf::from(d.trim());
        }
    }
    harness_core::config::base_dir("blastguard").join("blocks")
}

/// `<dir>/blocks.jsonl`.
pub fn log_path(dir: &Path) -> PathBuf {
    dir.join(FILE_NAME)
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

fn str_field(v: Option<&Value>, key: &str) -> Value {
    match v.and_then(|o| o.get(key)).and_then(Value::as_str) {
        Some(s) => Value::String(s.to_string()),
        None => Value::Null,
    }
}

/// Build the record for one block from the RAW stdin payload.
///
/// `parsed` says whether blastguard could read the payload as a hook input;
/// when it could not, the raw stdin is kept verbatim in `raw_payload` (it is
/// the only description of the blocked call there is).
pub fn record(raw: &str, parsed: bool, block: &Block<'_>, ts: u64) -> Value {
    let payload: Option<Value> = serde_json::from_str::<Value>(raw.trim())
        .ok()
        .filter(Value::is_object);
    let p = payload.as_ref();
    let tool_name = str_field(p, "tool_name");
    let tool_input = p.and_then(|o| o.get("tool_input")).cloned();
    let ti = tool_input.as_ref();

    let command = if tool_name.as_str() == Some("Bash") {
        str_field(ti, "command")
    } else {
        Value::Null
    };
    let file_path = match str_field(ti, "file_path") {
        Value::Null => str_field(ti, "notebook_path"),
        v => v,
    };

    let serialized = match &tool_input {
        Some(v) => serde_json::to_string(v).unwrap_or_default(),
        None => "null".to_string(),
    };
    let capped = serialized.len() > TOOL_INPUT_CAP_BYTES;
    let tool_input_value = if capped {
        Value::Null
    } else {
        tool_input.clone().unwrap_or(Value::Null)
    };

    let mut rec = json!({
        "v": VERSION,
        "ts": ts,
        "blastguard_version": env!("CARGO_PKG_VERSION"),
        "session_id": str_field(p, "session_id"),
        "agent_id": str_field(p, "agent_id"),
        "cwd": str_field(p, "cwd"),
        "tool_name": tool_name,
        "command": command,
        "file_path": file_path,
        "tool_input": tool_input_value,
        "tool_input_capped": capped,
        "tool_input_bytes": serialized.len(),
        "tool_input_sha256": sha_hex(serialized.as_bytes()),
        "decision": block.decision,
        "decision_kind": block.kind.as_str(),
        "original_decision": block.kind.original_decision(),
        "actor": block.actor,
        "system_harm": block.system_harm,
        "reason": block.reason,
        "rule_id": block.rule_id,
    });
    if !parsed {
        if let Some(o) = rec.as_object_mut() {
            o.insert("raw_payload".to_string(), Value::String(raw.to_string()));
        }
    }
    rec
}

/// Append one block record to `<dir>/blocks.jsonl` and return the path, or
/// the failure as text. Never silent: the caller must surface an `Err`.
pub fn write(dir: &Path, raw: &str, parsed: bool, block: &Block<'_>) -> Result<PathBuf, String> {
    let path = log_path(dir);
    let rec = record(raw, parsed, block, now());
    let line = serde_json::to_string(&rec).map_err(|e| format!("serialize: {e}"))?;
    harness_core::append::append_line(&path, &line)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// The text appended to the printed reason when the record could not be
/// written. Carries [`UNRECORDED_MARK`].
pub fn unrecorded_note(err: &str) -> String {
    format!(
        "\n\nblastguard: WARNING this block {UNRECORDED_MARK} in the blastguard block log \
         ({err}). Report the exact command (or file path and tool input) and cwd yourself; \
         there is no durable record of what was blocked."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(kind: DecisionKind) -> Block<'static> {
        Block {
            decision: "deny",
            kind,
            actor: "headless",
            system_harm: false,
            reason: "r",
            rule_id: "x",
        }
    }

    #[test]
    fn bash_command_is_kept_verbatim_and_hashed() {
        let cmd = "cat <<'EOF'\n\ta `b` \r\nEOF\nrm -rf \"$x\"  ";
        let raw = json!({"session_id":"s","cwd":"/c d","tool_name":"Bash",
                         "tool_input":{"command":cmd}})
        .to_string();
        let r = record(&raw, true, &block(DecisionKind::Deny), 7);
        assert_eq!(r["command"], cmd);
        assert_eq!(r["file_path"], Value::Null);
        assert_eq!(r["agent_id"], Value::Null);
        assert_eq!(r["tool_input"]["command"], cmd);
        assert_eq!(r["tool_input_capped"], false);
        let ser = serde_json::to_string(&json!({"command":cmd})).unwrap();
        assert_eq!(r["tool_input_sha256"], sha_hex(ser.as_bytes()));
        assert!(r.get("raw_payload").is_none());
    }

    #[test]
    fn oversized_tool_input_is_capped_but_hash_and_path_kept() {
        let big = "a".repeat(TOOL_INPUT_CAP_BYTES + 1);
        let raw =
            json!({"tool_name":"Write","tool_input":{"file_path":"/f","content":big}}).to_string();
        let r = record(&raw, true, &block(DecisionKind::Deny), 0);
        assert_eq!(r["tool_input_capped"], true);
        assert_eq!(r["tool_input"], Value::Null);
        assert_eq!(r["file_path"], "/f");
        assert_eq!(r["tool_input_sha256"].as_str().map(str::len), Some(64));
    }

    #[test]
    fn unparsed_payload_keeps_raw_stdin() {
        let r = record(
            "{not json",
            false,
            &block(DecisionKind::AskHardenedToDeny),
            0,
        );
        assert_eq!(r["raw_payload"], "{not json");
        assert_eq!(r["decision_kind"], "ask-hardened-to-deny");
        assert_eq!(r["original_decision"], "ask");
    }

    #[test]
    fn original_decision_is_explicit_for_every_kind() {
        let cases = [
            (DecisionKind::Deny, "deny"),
            (DecisionKind::Ask, "ask"),
            (DecisionKind::AskHardenedToDeny, "ask"),
            (
                DecisionKind::DenyDowngradedToAsk { hardened_ask: true },
                "ask",
            ),
            (
                DecisionKind::DenyDowngradedToAsk {
                    hardened_ask: false,
                },
                "deny",
            ),
            (DecisionKind::PanicBlocked, "undetermined"),
        ];
        for (kind, want) in cases {
            let r = record("{}", true, &block(kind), 0);
            assert_eq!(r["original_decision"], want, "{kind:?}: {r}");
        }
    }

    #[test]
    fn write_failure_is_returned() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f");
        std::fs::write(&file, "x").unwrap();
        assert!(write(&file, "{}", true, &block(DecisionKind::Deny)).is_err());
        assert!(unrecorded_note("e").contains(UNRECORDED_MARK));
    }
}
