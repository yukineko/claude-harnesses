//! File-tool (Write / Edit / MultiEdit / NotebookEdit) unit tests and the
//! entry-boundary unreadable-operand tests. Moved out of `tests.rs`
//! unchanged, which is at its file-growth cap.

use super::*;

// ---- File operations ----
#[test]
fn edit_is_always_allowed() {
    assert_eq!(
        detect("Edit", Some(&json!({ "file_path": "src/main.rs" }))),
        Decision::Allow
    );
    assert_eq!(
        detect("MultiEdit", Some(&json!({ "file_path": "src/main.rs" }))),
        Decision::Allow
    );
}

#[test]
fn write_empty_content_to_source_is_denied() {
    assert!(detect(
        "Write",
        Some(&json!({ "file_path": "src/main.rs", "content": "" }))
    )
    .is_deny());
    assert!(detect(
        "Write",
        Some(&json!({ "file_path": "src/main.rs", "content": "   \n" }))
    )
    .is_deny());
}

#[test]
fn write_with_content_or_to_config_is_allowed() {
    assert_eq!(
        detect(
            "Write",
            Some(&json!({ "file_path": "src/main.rs", "content": "fn main() {}" }))
        ),
        Decision::Allow
    );
    // Config files are exempt even when emptied.
    assert_eq!(
        detect(
            "Write",
            Some(&json!({ "file_path": "Cargo.toml", "content": "" }))
        ),
        Decision::Allow
    );
    // `.claude/settings.json` controls which hooks/gates run at all, so
    // wiping it must NOT fall through the config-file exemption to Allow.
    // See `write_to_protected_config_paths_is_gated` for the broader set of
    // security-relevant paths this covers.
    assert_protected_modify(detect(
        "Write",
        Some(&json!({ "file_path": ".claude/settings.json", "content": "" })),
    ));
}

#[test]
fn write_to_git_internals_is_denied() {
    assert!(detect(
        "Write",
        Some(&json!({ "file_path": ".git/config", "content": "x" }))
    )
    .is_deny());
}

/// An UNMATCHED tool is allowed — and only an unmatched tool.
///
/// This test used to be `missing_or_unknown_input_is_allowed` and asserted
/// three things under one name:
///
/// ```text
/// assert_eq!(detect("Bash", None), Decision::Allow);                  // removed
/// assert_eq!(detect("Read", Some(&json!({"file_path":"x"}))), Allow); // kept
/// assert_eq!(detect("Write", Some(&json!({}))), Decision::Allow);     // removed
/// ```
///
/// The middle one is a real specification: `Read` is not a tool blastguard
/// judges, so `Allow` is the accurate answer and folding it in would make
/// the hook prompt on every file read.
///
/// The other two were not a specification. They were the fail-open written
/// down AS one: `Bash` and `Write` ARE tools blastguard judges, and both
/// payloads are ones whose operand it could not read. Asserting `Allow`
/// there fixed "I could not analyse this" as "this is fine" — the exact
/// shape CLAUDE.md 2 cites in `assert!(checks_verdict(&[]))`, where a test
/// that verified nothing pinned an empty-set fail-open as the contract.
/// A test can only ever certify the behaviour it asserts; when the
/// behaviour is wrong, the test is a lock on the defect.
///
/// Those two cases did not lose coverage — they gained the opposite
/// assertion, in `a_bash_call_whose_command_cannot_be_read_is_asked_not_allowed`
/// and `a_write_whose_path_cannot_be_read_is_asked_not_allowed` below.
#[test]
fn an_unmatched_tool_is_allowed() {
    assert_eq!(
        detect("Read", Some(&json!({ "file_path": "x" }))),
        Decision::Allow
    );
    assert_eq!(
        detect("Grep", Some(&json!({ "pattern": "x" }))),
        Decision::Allow
    );
    assert_eq!(detect("WebFetch", None), Decision::Allow);
}

// ---- entry-boundary: unreadable OPERAND is not "nothing to judge" -------
//
// The distinction these pin, which `missing_or_unknown_input_is_allowed`
// above conflates into one rule:
//
//   * an UNMATCHED TOOL (Read, Grep, WebFetch) -> Allow is correct.
//     blastguard has no jurisdiction; there is genuinely nothing to judge.
//   * a MATCHED TOOL whose OPERAND cannot be read -> Allow is a fail-open.
//     blastguard has jurisdiction over this call and failed to exercise it.
//     That is "I don't understand this" recorded as "this is fine" — the
//     defect `model.rs:5` says the third answer exists to prevent, applied
//     to this crate's own front door.

#[test]
fn a_bash_call_whose_command_cannot_be_read_is_asked_not_allowed() {
    // No tool_input at all.
    assert!(detect("Bash", None).is_ask(), "Bash with no tool_input");
    // tool_input present but carrying no `command` key.
    assert!(
        detect("Bash", Some(&json!({}))).is_ask(),
        "Bash with no command key"
    );
    // `command` present but not a string — schema drift, or a payload
    // shaped to slip past the `as_str()`.
    assert!(
        detect("Bash", Some(&json!({ "command": 123 }))).is_ask(),
        "Bash with a non-string command"
    );
    assert!(
        detect("Bash", Some(&json!({ "command": ["rm", "-rf", "/"] }))).is_ask(),
        "Bash with an array command"
    );
}

#[test]
fn a_write_whose_path_cannot_be_read_is_asked_not_allowed() {
    assert!(detect("Write", None).is_ask(), "Write with no tool_input");
    assert!(
        detect("Write", Some(&json!({}))).is_ask(),
        "Write with no path key"
    );
    assert!(
        detect("Write", Some(&json!({ "file_path": 7 }))).is_ask(),
        "Write with a non-string path"
    );
    // An empty path string is equally unreadable: extract_path already
    // rejects it, and "" names no file to classify.
    assert!(
        detect("Write", Some(&json!({ "file_path": "" }))).is_ask(),
        "Write with an empty path"
    );
}

#[test]
fn an_edit_whose_path_cannot_be_read_is_asked_not_allowed() {
    for tool in ["Edit", "MultiEdit", "NotebookEdit"] {
        assert!(detect(tool, None).is_ask(), "{tool} with no tool_input");
        assert!(
            detect(tool, Some(&json!({}))).is_ask(),
            "{tool} with no path key"
        );
    }
}

/// ANTI-VACUITY CONTROL 1 — the DELIBERATE permissive spec.
///
/// A tool blastguard does not claim jurisdiction over must stay a silent
/// Allow. If the fix above were "Ask whenever anything is missing", this
/// would turn every Read and Grep in the session into a prompt, which is
/// not a stricter gate but a broken one.
#[test]
fn an_unmatched_tool_is_still_allowed_even_with_no_input() {
    for tool in ["Read", "Grep", "Glob", "WebFetch", "Task", "TodoWrite"] {
        assert_eq!(detect(tool, None), Decision::Allow, "{tool} with no input");
        assert_eq!(
            detect(tool, Some(&json!({ "file_path": "x" }))),
            Decision::Allow,
            "{tool} with input"
        );
    }
}

/// ANTI-VACUITY CONTROL 2 — a READABLE operand still gets its real verdict.
///
/// Proves the entry-boundary change did not short-circuit the analyser: an
/// ordinary command is still Allow and a destructive one is still Deny, so
/// the new Ask cannot be masking a blanket "Ask everything".
#[test]
fn a_readable_operand_still_reaches_the_real_analysis() {
    assert_eq!(
        detect("Bash", Some(&json!({ "command": "ls -la" }))),
        Decision::Allow
    );
    assert!(detect("Bash", Some(&json!({ "command": "rm -rf /" }))).is_deny());
    assert_eq!(
        detect(
            "Write",
            Some(&json!({ "file_path": "src/x.rs", "content": "fn main() {}" }))
        ),
        Decision::Allow
    );
    assert!(detect(
        "Write",
        Some(&json!({ "file_path": ".git/config", "content": "x" }))
    )
    .is_deny());
}
