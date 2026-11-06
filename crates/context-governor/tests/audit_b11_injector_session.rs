#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Audit B item 11 (memory `cg-injector-dedup-env-session`).
//!
//! The injector's dedup reads and writes the ledger of the session named by
//! the env var `CLAUDE_CODE_SESSION_ID`, not the session named by the hook
//! input's `session_id`. When the two differ, the dedup is answered by the
//! wrong session's ledger and the row lands in the wrong session's ledger.
//! Requirement: the hook input's session id decides which ledger is read and
//! written.
//!
//! This file holds ONE test, so its process-global env mutation cannot race
//! another test in the same binary.

use context_governor::defaults::DefaultInjector;
use context_governor::{ContextInjector, HookInput};

const DOC: &str = "# Rules\nAll API calls MUST include an Authorization header.\n\n# Examples\n```bash\ncurl -H 'Authorization: x' https://example.invalid\n```\n";

fn input(session: &str, cwd: &str) -> HookInput {
    serde_json::from_value(serde_json::json!({
        "session_id": session,
        "transcript_path": "",
        "cwd": cwd,
        "hook_event_name": "UserPromptSubmit",
        "prompt": "show me some examples",
    }))
    .unwrap()
}

fn injected(out: &context_governor::HookOutput) -> bool {
    out.specific
        .as_ref()
        .and_then(|s| s.additional_context.as_deref())
        .is_some_and(|t| !t.is_empty())
}

#[test]
#[ignore = "backlog 04f53dbe: open defect, remove ignore when fixed"]
fn injector_dedup_uses_the_hook_input_session_not_the_env_session() {
    let state = tempfile::tempdir().expect("state dir");
    let cwd_dir = tempfile::tempdir().expect("cwd");
    let cwd = cwd_dir.path().to_str().unwrap().to_string();
    let doc_dir = tempfile::tempdir().expect("doc dir");
    let doc = doc_dir.path().join("ref.md");
    std::fs::write(&doc, DOC).unwrap();

    std::env::set_var("CONTEXT_GOVERNOR_STATE_DIR", state.path());
    std::env::set_var("CONTEXT_GOVERNOR_REFERENCE_DOC", &doc);

    let sess_y = "audit-b11-input-y";
    let sess_x = "audit-b11-env-x";
    let ledger_y = harness_core::store::ledger_path(cwd_dir.path(), sess_y);
    let ledger_x = harness_core::store::ledger_path(cwd_dir.path(), sess_x);

    // CONTROL: env and input agree on Y. The first prompt injects and records
    // an `injected` row in Y's ledger.
    std::env::set_var("CLAUDE_CODE_SESSION_ID", sess_y);
    let first = DefaultInjector.inject(&input(sess_y, &cwd));
    assert!(injected(&first), "control: first prompt must inject");
    let y_rows = std::fs::read_to_string(&ledger_y).unwrap_or_default();
    assert!(
        y_rows.contains("\"injected\""),
        "control: the injection must be recorded in Y's ledger {}: {y_rows:?}",
        ledger_y.display()
    );

    // THE FINDING: the same prompt arrives for session Y while the env names a
    // different session X. Y already received this text, so the dedup must
    // suppress it, and X's ledger must not be touched.
    std::env::set_var("CLAUDE_CODE_SESSION_ID", sess_x);
    let second = DefaultInjector.inject(&input(sess_y, &cwd));
    let x_rows = std::fs::read_to_string(&ledger_x).unwrap_or_default();

    assert!(
        x_rows.is_empty(),
        "session X's ledger was written for a hook whose input names session Y: {} = {x_rows:?}",
        ledger_x.display()
    );
    assert!(
        !injected(&second),
        "session Y already received this text, but the injector re-injected it \
         (dedup was answered by the env session's ledger)"
    );
}
