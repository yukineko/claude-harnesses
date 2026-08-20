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
    // Entry boundary: the call was blastguard's to judge and its operand could
    // not be read. Its own id, and FIRST, because a recurring one means
    // something no other signature can say: blastguard's idea of the payload
    // schema has drifted from what Claude Code actually sends, and every call
    // of that shape is going unanalysed. That is a fleet-health signal about
    // the gate itself, not a verdict about any one command — filing it under a
    // command-shaped id would bury it.
    if reason.contains(crate::detect::UNREADABLE_OPERAND) {
        return "unreadable-operand";
    }
    // The location axis (`crate::scope`): a destructive shape whose every
    // target resolved strictly inside a safe root. ONE id for the whole class
    // (rm, find, truncate, shred, git clean, chmod/chown, `>` redirect) on
    // purpose: what recurs here is not "an rm was asked about", it is "work
    // inside this project keeps needing a confirmation", and that is the signal
    // worth seeing accumulate. Placed near the top because the reasons embed
    // the verb's own wording and would otherwise be filed under it.
    //
    // Keyed on the fixed phrase `detect::confined_ask` always emits. If that
    // wording changes and this does not, the id silently degrades to
    // "unknown" — which is why `confined_ask`'s doc comment names this
    // dependency in the other direction too.
    if reason.contains("is confined to") {
        return "scoped-destructive-confined";
    }
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
    //
    // ORDER IS LOAD-BEARING: the three ids below all contain the substring
    // "protected gate/config path", so the more specific wordings must be
    // tested BEFORE the generic one or every disarm would classify as a plain
    // write.
    //
    // Round 3 (disarm by non-write): neutralising a protected path WITHOUT
    // writing to it — `rm`, `mv` away, `chmod -x`, `git checkout --` — is a
    // different recurring failure from writing to it, and wants a different
    // signature: "an agent made the gate stop being read" vs "an agent edited
    // the gate".
    // Round 2 (unknown-verb catch-all). ALSO ordered before the generic arm:
    // this wording ends in "is a protected gate/config path" and would otherwise
    // be filed as a positive protected-path verdict. It is not one — it is a
    // refusal to guess about a verb blastguard has no rule for — and conflating
    // the two would hide exactly the signal this id exists to expose: a
    // RECURRING unknown verb aimed at the gates is the name of the next rule
    // this crate needs.
    if reason.contains("is a command blastguard has no rule for") {
        return "unknown-verb-protected-path";
    }
    if reason.contains("disarms a protected gate/config path") {
        return "protected-path-disarm";
    }
    // Round 2: the WILDCARD twin. A `*`-bearing operand cannot be matched
    // against the protected globs at all (it is not a literal path), so this
    // verdict is reached through the operand's literal PREFIX. Its own id
    // because a recurring one says something specific — an agent is reaching
    // for the gates through shell expansion, which no per-path rule can see.
    if reason.contains("expanding inside a protected gate/config tree") {
        return "protected-path-wildcard";
    }
    // Round 2: a `find -exec` placeholder (`{}`) in an unlinking position. Not a
    // verdict about the command — the operand is resolved by `find` at run time
    // — but a recurring one is the signal that find-exec is routinely being used
    // as a delete/move vector past the per-operand rules.
    if reason.contains("is a find -exec placeholder standing for every matched path") {
        return "find-exec-placeholder-operand";
    }
    // The CONTAINER twin: the target is the directory that holds protected
    // paths (`rm -rf .claude`), not a protected path itself.
    if reason.contains("destroys a directory tree holding protected gate/config paths") {
        return "protected-tree-destroy";
    }
    if reason.contains("protected gate/config path") {
        return "protected-path";
    }
    // The Ask twin of the two above: a recursive copy INTO a directory that
    // holds protected paths. Not a verdict about the command — the landing set
    // is not derivable from the command line — but a recurring one is the
    // signal that some real copy shape is routinely going unanalysed.
    if reason.contains("which holds protected gate/config paths") {
        return "protected-landing-unenumerable";
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
    // Round 2: plain `git clean -f`, without `-d`/`-x`. SAME id on purpose —
    // `-d` only widens the delete to directories, so this is one recurring
    // failure with two spellings, and splitting the signature would make the
    // class look half as frequent as it is.
    if reason.contains("git clean -f deletes untracked files irreversibly") {
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
    // Round 2: the pathspec-without-`--` spelling of the same whole-tree
    // discard. SAME id as the `--`-separated form: one operation, two
    // spellings, and the whole point of the round-2 fix was that they had
    // drifted apart.
    if reason.contains("git checkout . discards all working-tree changes") {
        return "git-checkout-dash-dot";
    }
    // Round 2: `git switch`, the other half git split `checkout` into.
    if reason.contains("git switch --force/--discard-changes") {
        return "git-switch-force";
    }
    // `git stash clear/drop`. Round 1 added the rule and did NOT add this arm,
    // so every stash-discard deny had been landing in the violation store as
    // "unknown" — the completeness obligation stated in the test below is real.
    if reason.contains("git stash clear/drop irreversibly deletes stashed changes") {
        return "git-stash-discard";
    }
    // Round 2: `git stash -u`/`-a`, which sweeps untracked files out of the
    // working tree. Its own id, not `git-stash-discard`: this one destroys
    // UNTRACKED work (git has no copy), which is the `git clean` hazard, while
    // clear/drop destroys already-stashed work.
    if reason.contains("git stash -u/-a removes untracked") {
        return "git-stash-untracked";
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
    // Kept separate from `unresolvable-command-word` even though both come out
    // of the same arm: this one says the head WAS resolved and the resolution
    // then lost the working directory an earlier `cd` had established, so the
    // segment's relative operands were judged against the wrong tree. That is a
    // gap in the resolver's own plumbing, not a property of the command, and it
    // has a different fix — folding it into the other id would hide how often
    // the resolution is being thrown away.
    if reason.contains("moved the working directory and this segment's relative operands") {
        return "resolved-head-lost-cwd";
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
            // Protected gate/hook/policy path: Write, Edit, and the redirect
            // forms all share the ONE "protected-path" rule id (see the
            // comment on that arm in `rule_id` above). Covering all four
            // call sites here pins that they actually reach it, not just
            // that the substring match exists.
            (
                "Write",
                json!({ "file_path": ".githooks/pre-commit", "content": "x" }),
            ),
            (
                "Edit",
                json!({ "file_path": ".claude/settings.json", "old_string": "a", "new_string": "b" }),
            ),
            (
                "Bash",
                json!({ "command": "echo x > .githooks/pre-commit" }),
            ),
            (
                "Bash",
                json!({ "command": "echo x >> .claude/settings.json" }),
            ),
            // git config core.hooksPath: the single-command equivalent of
            // overwriting every file under `.git/hooks/`.
            (
                "Bash",
                json!({ "command": "git config core.hooksPath .githooks" }),
            ),
            // Round 3 (disarm by non-write). Same completeness obligation as
            // every arm above: each NEW deny/ask wording added below has to be
            // exercised here, or it would classify as "unknown" in the
            // violation store while this test stayed green.
            ("Bash", json!({ "command": "rm .githooks/pre-commit" })),
            ("Bash", json!({ "command": "rm -rf .claude" })),
            (
                "Bash",
                json!({ "command": "mv .githooks/pre-commit /tmp/x" }),
            ),
            (
                "Bash",
                json!({ "command": "chmod -x .githooks/pre-commit" }),
            ),
            (
                "Bash",
                json!({ "command": "git checkout -- .claude/settings.json" }),
            ),
            ("Bash", json!({ "command": "tee -a .githooks/pre-commit" })),
            // The Ask twin: a recursive copy into a directory that HOLDS
            // protected paths.
            ("Bash", json!({ "command": "cp -r evildir/ .claude/" })),
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
            // Round 2 (adversarial verifier). Same completeness obligation:
            // each new wording below is exercised here, or it would classify as
            // "unknown" in the violation store while this test stayed green.
            ("Bash", json!({ "command": "git rm .githooks/pre-commit" })),
            ("Bash", json!({ "command": "unlink .claude/settings.json" })),
            ("Bash", json!({ "command": "rmdir .githooks" })),
            ("Bash", json!({ "command": "chmod 000 .claude/*" })),
            ("Bash", json!({ "command": "mv .claude/* /tmp/" })),
            ("Bash", json!({ "command": "git clean -f" })),
            ("Bash", json!({ "command": "git checkout ." })),
            ("Bash", json!({ "command": "git switch -f main" })),
            ("Bash", json!({ "command": "git stash clear" })),
            ("Bash", json!({ "command": "git stash -a" })),
            // The `..` bypass: same protected-path class, reached through a
            // spelling that used to defeat the match entirely.
            (
                "Write",
                json!({ "file_path": ".claude/x/../settings.json", "content": "x" }),
            ),
            // The Ask paths. Same completeness obligation as the denies: the
            // hook records an ask as a violation too, so an unclassified ask
            // would land in the store as "unknown".
            ("Bash", json!({ "command": "sh -c \"$MYSTERY --flag\"" })),
            (
                "Bash",
                json!({ "command": "not_a_real_wrapper_xyz rm -rf dir" }),
            ),
            ("Bash", json!({ "command": "gzip .claude/settings.json" })),
            (
                "Bash",
                json!({ "command": "find . -name pre-commit -exec mv {} /tmp/ ;" }),
            ),
            // Round 4 (adversarial verifier). Same completeness obligation:
            // each new deny/ask path is exercised here, or it would classify as
            // "unknown" in the violation store while this test stayed green.
            // `sort -o`/`uniq` output onto a protected path is a write to it,
            // so it reaches the shared protected-path deny.
            (
                "Bash",
                json!({ "command": "sort -o .githooks/pre-commit /dev/null" }),
            ),
            (
                "Bash",
                json!({ "command": "uniq /dev/null .claude/settings.json" }),
            ),
            // chmod removing the READ bit (not just exec) disarms a shell hook.
            (
                "Bash",
                json!({ "command": "chmod -r .githooks/pre-commit" }),
            ),
            (
                "Bash",
                json!({ "command": "chmod 311 .githooks/pre-commit" }),
            ),
            // rm -d/--dir on a protected container.
            ("Bash", json!({ "command": "rm -d .claude" })),
            // Round 5 (adversarial second author of the expansion-valued
            // command-word resolver). Resolving the head can lose a `cd` the
            // rest of the line established, so the arm refuses rather than
            // judging the operands against the wrong tree.
            (
                "Bash",
                json!({ "command": "cd .githooks; BIN=/bin/rm; $BIN pre-commit" }),
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
    fn a_resolution_that_lost_the_cwd_gets_its_own_id_not_the_unresolvable_one() {
        // `every_detect_deny_path_maps_to_a_known_rule_id` only proves this
        // reason is not "unknown"; it would stay green if the arm were folded
        // into `unresolvable-command-word`. The two say different things — one
        // is "the text does not name the program", the other is "it does, and I
        // threw away the directory it would have run in" — and `retro` counts
        // by id, so collapsing them would make the second invisible inside the
        // first. Pin the id itself.
        let reason = deny_reason(
            "Bash",
            json!({ "command": "cd .githooks; BIN=/bin/rm; $BIN pre-commit" }),
        );
        assert_eq!(
            rule_id(&reason),
            "resolved-head-lost-cwd",
            "reason: {reason:?}"
        );
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
