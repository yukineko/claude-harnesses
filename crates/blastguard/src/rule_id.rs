//! Stable rule identifiers for a [`crate::model::Decision::Deny`] reason.
//!
//! [`crate::detect`] produces human-facing free-text reasons (some of which
//! embed a variable path/target via `format!`, e.g. `"Write would replace
//! {path} with empty content, wiping the file"`). Free text is unsuitable as
//! an overwatch violation *signature* — the same recurring failure kind would
//! fragment into many distinct signatures depending on the path/command
//! involved. This module maps each fixed reason shape back to a small, stable
//! rule id (independent of the embedded variable text) so overwatch's
//! cross-task recurrence detection sees the same signature for "the same kind
//! of denial" every time.
//!
//! Matching is done by fixed-prefix / substring checks against the exact
//! wording [`crate::detect`] emits, kept deliberately in lockstep with it. An
//! unrecognized reason (e.g. after a future wording change was not mirrored
//! here) falls back to `"unknown"` rather than guessing — a stable but coarse
//! signature is safer than a spurious sig-per-wording split.

/// Reason surfaced when the analyser itself fails and the binary's panic
/// barrier turns the crash into a deny.
///
/// It deliberately says what happened rather than naming a rule: this is not a
/// verdict about the command, it is a refusal to guess about one.
///
/// Lives here, next to the classifier arm that recognises it, so the emitter
/// (`main.rs`) and the classifier cannot drift apart — the drift would be
/// silent, turning every crash into an `"unknown"` signature.
pub const INTERNAL_ERROR_REASON: &str =
    "blastguard hit an internal error while analysing this command — refusing rather than \
allowing it unanalysed. Re-run with a simpler command, or report this.";

/// Classify a deny `reason` string into a stable rule id.
pub fn rule_id(reason: &str) -> &'static str {
    // Write handling.
    if reason.contains("overwrite git internals") {
        return "write-git-internals";
    }
    if reason.contains("wiping the file") {
        return "write-empty-content";
    }
    // Protected gate/hook/policy path. Deliberately ONE id for the whole class
    // (Write, Edit/MultiEdit/NotebookEdit, truncating redirect, append
    // redirect): they are the same recurring failure — an agent reaching for
    // the file that decides whether the gates run — and splitting them by tool
    // would fragment exactly the signature overwatch needs to see recur.
    if reason.contains("protected gate/config path") {
        return "protected-path";
    }

    // Bash: fork bomb / redirect.
    if reason == "fork bomb pattern detected" {
        return "fork-bomb";
    }
    if reason.contains("truncates and overwrites an existing file") {
        return "truncating-redirect";
    }

    // Bash: rm.
    if reason.contains("recursive rm (-r) can delete an entire directory tree") {
        return "rm-recursive";
    }
    if reason.contains("rm with a wildcard can delete many files at once") {
        return "rm-wildcard";
    }

    // Bash: git.
    if reason.contains("git clean -f with -d/-x deletes untracked files") {
        return "git-clean-force";
    }
    if reason.contains("git reset --hard discards working-tree changes") {
        return "git-reset-hard";
    }
    if reason.contains("git checkout --force discards working-tree changes") {
        return "git-checkout-force";
    }
    if reason.contains("git checkout -- . discards all working-tree changes") {
        return "git-checkout-dash-dot";
    }
    if reason.contains("git config core.hooksPath repoints every git hook") {
        return "git-config-hookspath";
    }

    // Bash: truncate/shred/dd/chmod/chown/mkfs/find.
    if reason == "truncate can shrink a file to zero bytes" {
        return "truncate";
    }
    if reason == "shred destroys file contents irreversibly" {
        return "shred";
    }
    if reason == "dd with of= writes raw bytes over a device/file" {
        return "dd-of";
    }
    if reason == "recursive chmod re-permissions a whole tree" {
        return "chmod-recursive";
    }
    if reason == "recursive chown re-owns a whole tree" {
        return "chown-recursive";
    }
    if reason == "mkfs formats a filesystem, destroying all data" {
        return "mkfs";
    }
    if reason == "find -delete removes every matching file" {
        return "find-delete";
    }
    if reason.contains("find -exec on a shell can run an arbitrary destructive command") {
        return "find-exec-shell";
    }
    if reason == "find -exec rm removes every matching file" {
        return "find-exec-rm";
    }

    // CA-blastguard-010: code-interpreter inline-eval denials — both the
    // find-exec/-ok-wrapped path (analyze_find) and the bare top-level
    // command path (analyze_segment, CA-blastguard-006) — previously fell
    // through to "unknown", defeating cross-task recurrence detection for
    // this whole denial class. Both wordings share the "code interpreter"
    // and "inline-eval flag" phrasing, so a single substring match on
    // "interpreter" + "inline-eval flag" covers both without over-matching
    // any other reason in this file.
    if reason.contains("code interpreter") && reason.contains("inline-eval flag") {
        return "code-interpreter-inline-eval";
    }

    // Bash: tee.
    if reason.contains("tee without -a/--append truncates and overwrites") {
        return "tee-truncate";
    }

    // D4: the analysis-budget deny. This is not a verdict about the command —
    // it is a refusal to guess about one too complex to analyse — but it still
    // needs a stable id, because a recurring budget exhaustion is exactly the
    // signal that some real pattern is going unanalysed.
    if reason.contains("too complex to analyse within the safety budget") {
        return "analysis-budget-exhausted";
    }

    // The recursion-depth twin of the budget deny. Same character (a refusal to
    // guess, not a verdict about the command) and the same reason for needing a
    // stable id: a RECURRING depth exhaustion is the signal that some real
    // nesting shape is routinely going unanalysed. Kept separate from the
    // budget id on purpose — collapsing them would hide which of the two limits
    // is actually being hit, and they call for different fixes.
    if reason.contains("recursion depth limit") {
        return "analysis-depth-exhausted";
    }

    // The Ask paths. Like the budget deny these are not verdicts about the
    // command; they are refusals to guess about one. They still need stable ids
    // because a RECURRING ask is the signal that a real construct is routinely
    // going unanalysed — that frequency is only visible if the ids are stable.
    if reason.contains("whose value only exists at run time") {
        return "unresolvable-command-word";
    }
    if reason.contains("is not a command blastguard recognises") {
        return "unrecognised-wrapper";
    }

    // The analyser itself crashed. Distinct from every rule above: it says
    // nothing about the command, only that blastguard failed. A recurring one of
    // these is a bug report.
    if reason.contains("hit an internal error while analysing") {
        return "internal-error";
    }

    "unknown"
}

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
    use super::*;
    use crate::detect;
    use crate::model::Decision;
    use serde_json::json;

    /// Reason string of any BLOCKING verdict (deny or ask). Both kinds are
    /// recorded as violations by the hook, so both must classify to a stable id;
    /// accepting only `Deny` here would leave every ask path unexercised.
    fn deny_reason(tool: &str, input: serde_json::Value) -> String {
        match detect::detect(tool, Some(&input)) {
            Decision::Deny(reason) | Decision::Ask(reason) => reason,
            Decision::Allow => panic!("expected a blocking decision"),
        }
    }

    #[test]
    fn every_detect_deny_path_maps_to_a_known_rule_id() {
        let cases: Vec<(&str, serde_json::Value)> = vec![
            ("Bash", json!({ "command": "rm -rf dir" })),
            ("Bash", json!({ "command": "rm *" })),
            ("Bash", json!({ "command": "git clean -fdx" })),
            ("Bash", json!({ "command": "git reset --hard" })),
            ("Bash", json!({ "command": "git checkout --force" })),
            ("Bash", json!({ "command": "git checkout -- ." })),
            ("Bash", json!({ "command": "truncate -s0 x" })),
            ("Bash", json!({ "command": "shred secret" })),
            ("Bash", json!({ "command": "dd of=/dev/sda" })),
            ("Bash", json!({ "command": "chmod -R 777 ." })),
            ("Bash", json!({ "command": "chown -R root ." })),
            ("Bash", json!({ "command": "mkfs.ext4 /dev/sdb1" })),
            ("Bash", json!({ "command": "find . -delete" })),
            (
                "Bash",
                json!({ "command": "find . -type f -exec sh -c 'rm -rf {}' ;" }),
            ),
            ("Bash", json!({ "command": "find . -type f -exec rm {} ;" })),
            ("Bash", json!({ "command": "echo x > existing" })),
            ("Bash", json!({ "command": ":(){ :|:& };:" })),
            (
                "Write",
                json!({ "file_path": ".git/config", "content": "x" }),
            ),
            (
                "Write",
                json!({ "file_path": "src/main.rs", "content": "" }),
            ),
            // D4 analysis-budget deny. This case was missing when the budget
            // was introduced, so this test passed while the new deny path
            // classified to "unknown" — the test asserted completeness it did
            // not actually have. Any future deny path must be added here too.
            (
                "Bash",
                json!({ "command": format!(
                    "find . {}-exec echo {{}} {}",
                    "-exec find . ".repeat(32),
                    "+ ".repeat(33)
                ) }),
            ),
            // The Ask paths. Same completeness obligation as the denies: the
            // hook records an ask as a violation too, so an unclassified ask
            // would land in the store as "unknown".
            ("Bash", json!({ "command": "sh -c \"$MYSTERY --flag\"" })),
            (
                "Bash",
                json!({ "command": "not_a_real_wrapper_xyz rm -rf dir" }),
            ),
        ];

        for (tool, input) in cases {
            let reason = deny_reason(tool, input.clone());
            let id = rule_id(&reason);
            assert_ne!(
                id, "unknown",
                "reason {reason:?} (from {tool} {input}) did not classify to a stable rule id"
            );
        }
    }

    #[test]
    fn the_internal_error_reason_classifies_to_its_own_id() {
        // Asserted against the shared constant the binary actually emits, not
        // against a copy of its text: a reword that broke the classifier would
        // otherwise leave this test green while every crash landed in the
        // violation store as "unknown".
        assert_eq!(rule_id(INTERNAL_ERROR_REASON), "internal-error");
    }

    #[test]
    fn same_rule_id_regardless_of_embedded_variable_text() {
        // Two different paths hitting the same detect branch must normalize
        // to the identical rule id — proving the signature is stable across
        // the variable part of the reason string.
        let r1 = deny_reason("Write", json!({ "file_path": "a.rs", "content": "" }));
        let r2 = deny_reason(
            "Write",
            json!({ "file_path": "b/c/d.rs", "content": "   " }),
        );
        assert_ne!(r1, r2, "reasons embed different paths");
        assert_eq!(rule_id(&r1), rule_id(&r2));
        assert_eq!(rule_id(&r1), "write-empty-content");
    }

    #[test]
    fn unrecognized_reason_falls_back_to_unknown() {
        assert_eq!(rule_id("some brand new denial wording"), "unknown");
    }

    // ---- Regression: CA-blastguard-010 (interpreter-deny reasons normalized to "unknown") ----
    #[test]
    fn ca_blastguard_010_interpreter_deny_reasons_map_to_stable_rule_id() {
        // Both the find-exec-wrapped path and the bare top-level path (added
        // for CA-blastguard-006) must classify to the SAME distinct, stable
        // rule id — not "unknown" — so cross-task recurrence detection sees
        // the same signature for this whole denial class.
        let wrapped = deny_reason(
            "Bash",
            json!({ "command": "find . -exec python3 -c \"import os; os.system('rm -rf /')\" \\;" }),
        );
        let bare = deny_reason(
            "Bash",
            json!({ "command": "python3 -c \"import os; os.system('rm -rf /')\"" }),
        );
        assert_ne!(rule_id(&wrapped), "unknown");
        assert_ne!(rule_id(&bare), "unknown");
        assert_eq!(rule_id(&wrapped), rule_id(&bare));
        assert_eq!(rule_id(&wrapped), "code-interpreter-inline-eval");
    }
}
