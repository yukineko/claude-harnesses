//! Coexistence contract: `context-governor` (CG) composes with `ctxrot` without
//! conflict on the four shared hook events (PostToolUse, UserPromptSubmit,
//! SessionStart, PreCompact).
//!
//! The companion doc — `docs/coexistence-with-ctxrot.md` — argues *why* the two
//! plugins don't conflict (field-disjoint writes; both side-effect-only on
//! PreCompact; additive on UserPromptSubmit/SessionStart). This test locks in
//! CG's *half* of that contract, end-to-end through the real dispatch binary:
//!
//!   On every shared event, CG **emits a valid hook envelope and exits 0** — it
//!   never blocks (the sole permitted block is a `PreCompact` *Block*, which the
//!   default `CompactionGuard` never produces). And it does this with **no ctxrot
//!   binary present and no ctxrot state on disk**, so CG provably neither depends
//!   on nor clobbers ctxrot's state.
//!
//! The test is self-contained: it drives the compiled `context-governor` binary
//! (via `CARGO_BIN_EXE_context-governor`) over stdin, in an isolated state dir,
//! and asserts on the process's exit status + stdout envelope. There is no
//! dependency on the `ctxrot` crate or binary.

use std::io::Write as _;
use std::process::{Command, Stdio};

/// Run the compiled `context-governor` binary with `payload` on stdin and a
/// freshly-isolated `CONTEXT_GOVERNOR_STATE_DIR` (so the test never touches
/// `$HOME` or any shared store). Returns `(exit_code, stdout)`.
///
/// `extra_env` lets a scenario set CG-only env vars (e.g. the reference doc).
/// Crucially, **no ctxrot env var is ever set** — CG must behave identically
/// whether or not ctxrot is installed.
fn run_cg(payload: &serde_json::Value, extra_env: &[(&str, &str)]) -> (i32, String) {
    let state_dir = tempfile::tempdir().expect("state dir");

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_context-governor"));
    cmd.env("CONTEXT_GOVERNOR_STATE_DIR", state_dir.path())
        // A deterministic session id keeps any ledger writes inside our temp dir.
        .env("CLAUDE_CODE_SESSION_ID", "coexistence-test")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }

    let mut child = cmd.spawn().expect("spawn context-governor");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(payload.to_string().as_bytes())
        .expect("write payload");
    let out = child.wait_with_output().expect("wait for context-governor");

    let code = out.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    // Keep `state_dir` alive until after the process exits.
    drop(state_dir);
    (code, stdout)
}

/// A stdout line is a valid CG envelope iff it is either the empty-object no-op
/// (`{}`) or a JSON object carrying `hookSpecificOutput`. Both are envelopes
/// Claude Code accepts as "proceed".
fn is_valid_envelope(stdout: &str) -> bool {
    let v: serde_json::Value = match serde_json::from_str(stdout) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let obj = match v.as_object() {
        Some(o) => o,
        None => return false,
    };
    obj.is_empty() || obj.contains_key("hookSpecificOutput")
}

/// PostToolUse: CG's groomer rewrites `updatedToolOutput`; ctxrot's toolguard
/// only adds `additionalContext`. Field-disjoint → compose. Here CG must emit a
/// valid envelope and exit 0 for both a small (no-op) and an oversized (groomed)
/// tool result, regardless of any ctxrot state.
#[test]
fn post_tool_use_emits_valid_envelope_and_exits_zero() {
    // Small result → CG declines to groom → `{}` no-op.
    let small = serde_json::json!({
        "session_id": "coexistence-test",
        "transcript_path": "",
        "cwd": "",
        "hook_event_name": "PostToolUse",
        "tool_name": "Bash",
        "tool_response": "a small tool result",
    });
    let (code, stdout) = run_cg(&small, &[]);
    assert_eq!(code, 0, "PostToolUse must exit 0; stdout: {stdout}");
    assert!(is_valid_envelope(&stdout), "invalid envelope: {stdout}");

    // Oversized result → CG grooms → a PostToolUse `updatedToolOutput` envelope.
    let big = serde_json::json!({
        "session_id": "coexistence-test",
        "transcript_path": "",
        "cwd": "",
        "hook_event_name": "PostToolUse",
        "tool_name": "Bash",
        "tool_response": "z".repeat(40_000), // ~10k tokens, well over the 2048 default
    });
    let (code, stdout) = run_cg(&big, &[]);
    assert_eq!(
        code, 0,
        "PostToolUse (oversized) must exit 0; stdout: {stdout}"
    );
    assert!(is_valid_envelope(&stdout), "invalid envelope: {stdout}");
    assert!(
        stdout.contains("updatedToolOutput"),
        "oversized groom must rewrite updatedToolOutput (the field ctxrot's \
         toolguard never touches); got: {stdout}"
    );
    // CG must NOT emit the field ctxrot owns on this event — the two are disjoint.
    assert!(
        !stdout.contains("additionalContext"),
        "CG's PostToolUse groomer must not write additionalContext (ctxrot's \
         field); got: {stdout}"
    );
}

/// UserPromptSubmit: both plugins only *add* context. CG injects a reference
/// section as `additionalContext`; ctxrot prints band/large-ref advice. CG must
/// emit a valid envelope and exit 0 whether or not its reference doc is set.
#[test]
fn user_prompt_submit_emits_valid_envelope_and_exits_zero() {
    // No reference doc configured → CG injects nothing → `{}`.
    let payload = serde_json::json!({
        "session_id": "coexistence-test",
        "transcript_path": "",
        "cwd": "",
        "hook_event_name": "UserPromptSubmit",
        "prompt": "show me some examples",
    });
    let (code, stdout) = run_cg(&payload, &[]);
    assert_eq!(code, 0, "UserPromptSubmit must exit 0; stdout: {stdout}");
    assert!(is_valid_envelope(&stdout), "invalid envelope: {stdout}");
    assert_eq!(stdout, "{}", "no reference doc → no-op; got: {stdout}");

    // With a reference doc → CG injects an `additionalContext` envelope. It must
    // never replace the prompt (no `updatedToolOutput` on this event).
    let mut doc = tempfile::NamedTempFile::new().expect("reference doc");
    doc.write_all(b"# Examples\n```bash\ncurl https://api.example.com/users\n```\n")
        .expect("write doc");
    doc.flush().expect("flush");
    let doc_path = doc.path().to_str().expect("utf-8 doc path");

    let (code, stdout) = run_cg(&payload, &[("CONTEXT_GOVERNOR_REFERENCE_DOC", doc_path)]);
    assert_eq!(
        code, 0,
        "UserPromptSubmit (with doc) must exit 0; stdout: {stdout}"
    );
    assert!(is_valid_envelope(&stdout), "invalid envelope: {stdout}");
    assert!(
        stdout.contains("additionalContext"),
        "matching prompt must inject additionalContext; got: {stdout}"
    );
    assert!(
        !stdout.contains("updatedToolOutput"),
        "UserPromptSubmit injection must only ADD context, never replace the \
         prompt; got: {stdout}"
    );
}

/// SessionStart: CG's rehydrator recalls its own `SNAPSHOT_KEY` (additive);
/// ctxrot's restore injects its own carryover note. With no CG snapshot and no
/// ctxrot state present, CG must emit a valid envelope (here the empty no-op)
/// and exit 0 — it neither depends on nor reads ctxrot's note store.
#[test]
fn session_start_emits_valid_envelope_and_exits_zero() {
    let td = tempfile::tempdir().expect("cwd");
    let payload = serde_json::json!({
        "session_id": "coexistence-test",
        "transcript_path": "",
        "cwd": td.path().to_str().expect("utf-8 cwd"),
        "hook_event_name": "SessionStart",
        "source": "startup",
    });
    let (code, stdout) = run_cg(&payload, &[]);
    assert_eq!(code, 0, "SessionStart must exit 0; stdout: {stdout}");
    assert!(is_valid_envelope(&stdout), "invalid envelope: {stdout}");
    assert_eq!(
        stdout, "{}",
        "no CG snapshot present → rehydrator no-op (it never falls back to ctxrot \
         state); got: {stdout}"
    );
}

/// SessionStart on `source == "compact"`: crash-freedom only. The binary is
/// invoked directly (bypassing the hooks.json matcher) against an EMPTY store,
/// so this proves just that CG emits a valid envelope and exits 0 on a compact
/// source. It does NOT prove routing or rehydration; those are covered by
/// `tests/backlog_565fb2a8.rs`.
#[test]
fn session_start_on_compact_source_exits_zero() {
    let td = tempfile::tempdir().expect("cwd");
    let payload = serde_json::json!({
        "session_id": "coexistence-test",
        "transcript_path": "",
        "cwd": td.path().to_str().expect("utf-8 cwd"),
        "hook_event_name": "SessionStart",
        "source": "compact",
    });
    let (code, stdout) = run_cg(&payload, &[]);
    assert_eq!(
        code, 0,
        "SessionStart(compact) must exit 0; stdout: {stdout}"
    );
    assert!(is_valid_envelope(&stdout), "invalid envelope: {stdout}");
}

/// PreCompact: both plugins are pure side effects and neither blocks. CG's guard
/// snapshots the transcript into its own backing store and returns `Proceed`
/// (exit 0, `{}`); ctxrot's rescue writes a markdown note. With a real
/// transcript, CG must still exit 0 — it never returns the (only-permitted)
/// Block, and it writes to its own sink, not ctxrot's.
#[test]
fn pre_compact_proceeds_and_exits_zero() {
    let mut tf = tempfile::NamedTempFile::new().expect("transcript");
    writeln!(
        tf,
        r#"{{"message":{{"role":"user","content":"hello from the user turn"}}}}"#
    )
    .unwrap();
    writeln!(
        tf,
        r#"{{"message":{{"role":"assistant","content":"reply from the assistant turn"}}}}"#
    )
    .unwrap();
    tf.flush().unwrap();

    let td = tempfile::tempdir().expect("cwd");
    let payload = serde_json::json!({
        "session_id": "coexistence-test",
        "transcript_path": tf.path().to_str().expect("utf-8 transcript path"),
        "cwd": td.path().to_str().expect("utf-8 cwd"),
        "hook_event_name": "PreCompact",
        "trigger": "manual",
    });
    let (code, stdout) = run_cg(&payload, &[]);
    assert_eq!(
        code, 0,
        "PreCompact default guard must Proceed (exit 0), never Block; stdout: {stdout}"
    );
    assert!(is_valid_envelope(&stdout), "invalid envelope: {stdout}");
}

/// PreCompact on a *missing* transcript: the guard's snapshot is fail-soft, so
/// it must still Proceed (exit 0) rather than panic or block. This is the
/// degenerate path ctxrot's rescue also tolerates (it returns `None`), so the
/// two stay non-blocking together.
#[test]
fn pre_compact_with_missing_transcript_still_proceeds() {
    let td = tempfile::tempdir().expect("cwd");
    let payload = serde_json::json!({
        "session_id": "coexistence-test",
        "transcript_path": "/no/such/transcript.jsonl",
        "cwd": td.path().to_str().expect("utf-8 cwd"),
        "hook_event_name": "PreCompact",
        "trigger": "auto",
    });
    let (code, stdout) = run_cg(&payload, &[]);
    assert_eq!(
        code, 0,
        "PreCompact must Proceed even when the transcript is missing; stdout: {stdout}"
    );
    assert!(is_valid_envelope(&stdout), "invalid envelope: {stdout}");
}

/// Cross-cutting: across all four shared events, CG never exits non-zero. This
/// is the single invariant the coexistence doc leans on — "CG never blocks
/// except a PreCompact Block, which the defaults never produce". Driving the
/// real binary with one representative payload per event, none ever blocks.
#[test]
fn no_shared_event_ever_blocks() {
    let td = tempfile::tempdir().expect("cwd");
    let cwd = td.path().to_str().expect("utf-8 cwd");

    let payloads = [
        serde_json::json!({
            "session_id": "coexistence-test", "transcript_path": "", "cwd": cwd,
            "hook_event_name": "PostToolUse", "tool_name": "Read", "tool_response": "tiny",
        }),
        serde_json::json!({
            "session_id": "coexistence-test", "transcript_path": "", "cwd": cwd,
            "hook_event_name": "UserPromptSubmit", "prompt": "anything",
        }),
        serde_json::json!({
            "session_id": "coexistence-test", "transcript_path": "", "cwd": cwd,
            "hook_event_name": "SessionStart", "source": "resume",
        }),
        serde_json::json!({
            "session_id": "coexistence-test", "transcript_path": "", "cwd": cwd,
            "hook_event_name": "PreCompact", "trigger": "manual",
        }),
    ];

    for p in &payloads {
        let event = p["hook_event_name"].as_str().unwrap_or("?");
        let (code, stdout) = run_cg(p, &[]);
        assert_eq!(
            code, 0,
            "{event} must exit 0 (CG never blocks); stdout: {stdout}"
        );
        assert!(
            is_valid_envelope(&stdout),
            "{event} produced an invalid envelope: {stdout}"
        );
    }
}
