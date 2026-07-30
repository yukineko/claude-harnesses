// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Round 2 of the adversarial review of the protected-path rule (blastguard
//! 0.2.19 -> 0.2.20). Every scenario here was OBSERVED to be `Allow` on the
//! shipped binary before this file existed; each one reaches a protected
//! gate/hook/policy path through a spelling no rule classified.
//!
//! Written as an integration test on purpose: the findings are about the
//! END-TO-END verdict a tool call receives, not about any one helper. Calling
//! `detect::detect` is the same entry point `main.rs` uses.
//!
//! Every group carries BOTH controls — a known-DENY and a known-ALLOW — because
//! an earlier round of this review produced a false all-clear from a stale
//! binary: with no control, "everything returned Allow" is indistinguishable
//! from "the probe never reached the code under test".

use blastguard::detect::detect;
use blastguard::exclude;
use blastguard::model::Decision;
use serde_json::json;

fn bash(cmd: &str) -> Decision {
    detect("Bash", Some(&json!({ "command": cmd })))
}

fn write(path: &str) -> Decision {
    detect("Write", Some(&json!({ "file_path": path, "content": "x" })))
}

fn edit(path: &str) -> Decision {
    detect(
        "Edit",
        Some(&json!({ "file_path": path, "old_string": "a", "new_string": "b" })),
    )
}

/// A blocking verdict is Deny OR Ask: both stop the call from running
/// unreviewed, and `Decision::hardened` collapses Ask to Deny wherever no human
/// is present. Assertions that only care "this no longer sails through" use
/// this; assertions about a positively recognised hazard assert `is_deny`.
fn assert_blocking(d: Decision, what: &str) {
    assert!(
        d.is_blocking(),
        "expected a blocking verdict for `{what}`, got {d:?}"
    );
}

// Two verdicts, asserted inline rather than through a helper
// ---------------------------------------------------------------------------
// MODIFYING a protected path is an `Ask` where a human can answer, hardening to
// `Deny` where none can; DESTROYING or DISARMING one stays a flat `Deny`.
// blastguard can establish that the target decides whether a gate runs, but not
// whether the incoming bytes arm or disarm it — different facts, so different
// verdicts. Every modify site asserts BOTH halves, which is strictly more than
// the single `is_deny()` it replaces.
//
// These stayed inline instead of moving into an `assert_modify` helper on
// purpose. scripts/check-test-weakening.py counts assertion macros textually, so
// extracting them into a helper reads to that gate as coverage being deleted
// (it measured 62 -> 57 here while the assertions actually EVALUATED rose
// 15 -> 25). Inline they are visible to the scanner and carry exact line numbers
// on failure. See the sibling `assert_blocking`, which predates this and is
// unaffected because it never reduced the count.

// ---------------------------------------------------------------------------
// Controls. If these ever fail, no other assertion in this file means anything.
// ---------------------------------------------------------------------------

#[test]
fn controls_deny_side() {
    let d = write(".claude/settings.json");
    assert!(d.is_ask(), "write settings.json must Ask, got {d:?}");
    assert!(
        d.clone().hardened().is_deny(),
        "write settings.json must harden to Deny, got {d:?}"
    );

    let d = write("/Users/yuki/.claude/settings.json");
    assert!(
        d.is_ask(),
        "write ~/.claude/settings.json must Ask, got {d:?}"
    );
    assert!(
        d.clone().hardened().is_deny(),
        "write ~/.claude/settings.json must harden to Deny, got {d:?}"
    );

    assert!(
        bash("rm -rf .githooks").is_deny(),
        "rm -rf .githooks destroys the gate; it stays a flat Deny"
    );
}

#[test]
fn controls_allow_side() {
    assert_eq!(write("src/main.rs"), Decision::Allow);
    assert_eq!(edit("src/main.rs"), Decision::Allow);
    assert_eq!(bash("rm build/x.o"), Decision::Allow);
    assert_eq!(bash("mv src/a.rs src/b.rs"), Decision::Allow);
    assert_eq!(bash("chmod +x scripts/run.sh"), Decision::Allow);
    assert_eq!(bash("cp a.txt b.txt"), Decision::Allow);
    assert_eq!(bash("git checkout main"), Decision::Allow);
    assert_eq!(bash("git checkout -b foo"), Decision::Allow);
    // `cp` only READS its source, so backing a protected file up is fine.
    assert_eq!(
        bash("cp .claude/settings.json /tmp/backup"),
        Decision::Allow
    );
}

// ---------------------------------------------------------------------------
// FINDING 1 — `..` moved a path to the PERMISSIVE side.
//
// A `..` segment breaks the multi-component protected globs
// (`.claude/settings.json`) while the allowlist's `.claude/**` still matches,
// so the path lost its protection AND kept its exemption.
// ---------------------------------------------------------------------------

#[test]
fn dotdot_is_resolved_lexically_by_normalize() {
    assert_eq!(
        exclude::normalize(".claude/x/../settings.json"),
        ".claude/settings.json"
    );
    assert_eq!(
        exclude::normalize("/Users/yuki/.claude/plugins/../settings.json"),
        "/Users/yuki/.claude/settings.json"
    );
    assert_eq!(exclude::normalize("a/b/../../c"), "c");
    // A `..` that escapes the root must NOT silently vanish.
    assert_eq!(exclude::normalize("../a"), "../a");
    assert_eq!(exclude::normalize("a/../../b"), "../b");
}

#[test]
fn dotdot_paths_still_match_the_protected_set() {
    assert!(exclude::is_protected_path(".claude/x/../settings.json"));
    assert!(exclude::is_protected_path(
        "/Users/yuki/.claude/plugins/../settings.json"
    ));
    assert!(exclude::is_protected_path(".claude/hooks/../hooks.json"));
    assert!(exclude::is_protected_path("foo/../.githooks/pre-commit"));
    // Resolution must never REMOVE protection that the unresolved spelling had.
    assert!(exclude::is_protected_path(".claude/hooks/sub/.."));
}

#[test]
fn a_residual_dotdot_fails_the_allowlist_too() {
    // The root cause of finding 1 was ASYMMETRY: a `..` broke the protected
    // match while leaving the allowlist match intact. A path this module cannot
    // resolve must fail BOTH lists, never just the restrictive one.
    assert!(!exclude::is_config_file("../foo/Cargo.toml"));
    assert!(!exclude::is_config_file("../.claude/agents/x.md"));
    // Control: the same paths without the escape are still ordinary config.
    assert!(exclude::is_config_file("foo/Cargo.toml"));
    assert!(exclude::is_config_file(".claude/agents/x.md"));
}

#[test]
fn dotdot_write_and_edit_are_gated() {
    // What this pins is that the `..` spelling does not HIDE the target from
    // the rule. Which blocking verdict follows is a separate question: these are
    // writes, not deletions, so both halves of the modify contract are asserted.
    for (d, what) in [
        (write(".claude/x/../settings.json"), "write ..-spelled"),
        (
            write("/Users/yuki/.claude/plugins/../settings.json"),
            "write ..-spelled abs",
        ),
        (edit(".claude/x/../settings.json"), "edit ..-spelled"),
        (
            edit("/Users/yuki/.claude/plugins/../settings.json"),
            "edit ..-spelled abs",
        ),
    ] {
        assert!(
            d.is_ask(),
            "modifying a protected path must Ask (refusal to guess) for `{what}`, got {d:?}"
        );
        assert!(
            d.clone().hardened().is_deny(),
            "with no human present `{what}` must harden to Deny, got {d:?}"
        );
    }
}

#[test]
fn dotdot_bash_operands_are_gated() {
    // Shapes whose DESTINATION is the protected path: bytes land on it, and what
    // those bytes do to the gate is not derivable from the command line.
    for cmd in [
        "cp /tmp/evil .claude/a/../settings.json",
        "install /tmp/evil .claude/a/../settings.json",
        "sed -i s/a/b/ .claude/x/../settings.json",
        "tee -a .claude/x/../settings.json",
    ] {
        let d = bash(cmd);
        assert!(
            d.is_ask(),
            "modifying a protected path must Ask (refusal to guess) for `{cmd}`, got {d:?}"
        );
        assert!(
            d.clone().hardened().is_deny(),
            "with no human present `{cmd}` must harden to Deny, got {d:?}"
        );
    }
    // Shapes that REMOVE or NEUTRALISE the protected path. The gate stops
    // running either way, which the command establishes on its own.
    for cmd in [
        "rm .claude/x/../settings.json",
        "rm -rf .claude/x/../hooks",
        "mv .claude/x/../settings.json /tmp/",
        "chmod -x .claude/hooks/x/../pre.sh",
    ] {
        assert!(
            bash(cmd).is_deny(),
            "destroying/disarming a protected path stays a Deny for `{cmd}`"
        );
    }
}

// ---------------------------------------------------------------------------
// FINDING 2 — `git rm` was entirely unclassified.
// ---------------------------------------------------------------------------

#[test]
fn git_rm_of_protected_paths_is_denied() {
    for cmd in [
        "git rm .githooks/pre-commit",
        "git rm -rf .githooks",
        "git rm -r -f .claude",
        "git rm --cached .claude/settings.json",
        "git rm -f .claude/hooks/on-stop.sh",
        "git -C . rm .githooks/pre-commit",
    ] {
        assert!(bash(cmd).is_deny(), "expected Deny for `{cmd}`");
    }
}

#[test]
fn git_rm_of_ordinary_files_stays_allowed() {
    assert_eq!(bash("git rm src/old.rs"), Decision::Allow);
    assert_eq!(bash("git rm -r target"), Decision::Allow);
    assert_eq!(bash("git status"), Decision::Allow);
}

// ---------------------------------------------------------------------------
// FINDING 3 — the unknown-verb catch-all waved whole families through.
// ---------------------------------------------------------------------------

#[test]
fn unlink_and_rmdir_of_protected_paths_are_denied() {
    for cmd in [
        "unlink .githooks/pre-commit",
        "unlink .claude/settings.json",
        "rmdir .githooks",
        "rmdir .claude/hooks",
    ] {
        assert!(bash(cmd).is_deny(), "expected Deny for `{cmd}`");
    }
    // Ordinary targets keep working.
    assert_eq!(bash("unlink build/tmp.o"), Decision::Allow);
    assert_eq!(bash("rmdir build/empty"), Decision::Allow);
}

#[test]
fn an_unrecognised_verb_touching_a_protected_path_blocks() {
    for cmd in [
        "patch -p1 .claude/settings.json",
        "ed -s .githooks/pre-commit",
        "ex -s .githooks/pre-commit",
        "awk -i inplace 1 .claude/settings.json",
        "gzip .claude/settings.json",
        "bzip2 .githooks/pre-commit",
        "xz .claude/hooks.json",
        "rsync -a --delete /tmp/empty/ .claude/hooks/",
        "ditto /tmp/evil .claude/hooks",
        "tar -xf evil.tar -C .claude",
        "unzip -o evil.zip -d .githooks",
        "perl -i -pe s/a/b/ .githooks/pre-commit",
        "touch .githooks/pre-commit",
    ] {
        assert_blocking(bash(cmd), cmd);
    }
}

#[test]
fn read_only_verbs_on_protected_paths_stay_allowed() {
    // The exemption list is the whole reason the rule above is affordable:
    // READING a gate's config is routine and harmless.
    for cmd in [
        "cat .claude/settings.json",
        "grep hooks .claude/settings.json",
        "rg hooks .githooks/pre-commit",
        "head -20 .githooks/pre-commit",
        "wc -l .githooks/pre-commit",
        "ls .githooks",
        "ls -la .claude/hooks",
        "jq . .claude/settings.json",
        "diff .claude/settings.json /tmp/other.json",
        "stat .githooks/pre-commit",
        "cd .githooks",
        "mkdir -p .claude/hooks",
        "echo hello",
        "cargo build",
    ] {
        assert_eq!(bash(cmd), Decision::Allow, "expected Allow for `{cmd}`");
    }
}

// ---------------------------------------------------------------------------
// FINDING 5 — "a wildcard operand is denied anyway" was true only inside `rm`.
// ---------------------------------------------------------------------------

#[test]
fn wildcards_aimed_into_a_protected_tree_are_blocked() {
    for cmd in [
        "chmod 000 .claude/*",
        "chmod 644 .claude/*",
        "chmod -x .claude/hooks/*",
        "mv .claude/* /tmp/",
        "mv .githooks/* /tmp/",
        "git rm .claude/*",
    ] {
        assert_blocking(bash(cmd), cmd);
    }
}

#[test]
fn wildcards_outside_protected_trees_stay_allowed() {
    assert_eq!(bash("chmod 755 .claude/*"), Decision::Allow); // grants exec
    assert_eq!(bash("chmod +x scripts/*.sh"), Decision::Allow);
    assert_eq!(bash("chmod 644 src/*.rs"), Decision::Allow);
    assert_eq!(bash("mv src/* /tmp/"), Decision::Allow);
}

// ---------------------------------------------------------------------------
// ALSO FIX — git verbs whose destructive spellings had no arm.
// ---------------------------------------------------------------------------

#[test]
fn git_stash_untracked_forms_are_denied() {
    for cmd in [
        "git stash -a",
        "git stash --all",
        "git stash push -u",
        "git stash save -u",
        "git stash push --include-untracked",
    ] {
        assert!(bash(cmd).is_deny(), "expected Deny for `{cmd}`");
    }
    assert_eq!(bash("git stash list"), Decision::Allow);
    assert_eq!(bash("git stash push -m wip"), Decision::Allow);
    assert!(bash("git stash clear").is_deny());
}

#[test]
fn git_checkout_pathspec_without_dashdash_is_denied() {
    for cmd in [
        "git checkout .",
        "git checkout HEAD .",
        "git -C . checkout .",
        "git checkout ./",
    ] {
        assert!(bash(cmd).is_deny(), "expected Deny for `{cmd}`");
    }
    assert_eq!(bash("git checkout main"), Decision::Allow);
    assert_eq!(bash("git checkout -b feature/x"), Decision::Allow);
}

#[test]
fn git_checkout_dashdash_ordinary_path_stays_allowed_protected_denied() {
    // Over-block narrowing: `git checkout -- <ordinary tracked file>` is the
    // everyday "discard my uncommitted edits to this one file" operation and
    // must NOT be denied (an over-block auditor caught round 1 blanket-denying
    // it — the friction that gets a gate turned off). It was reported RED
    // (deny) before this narrowing.
    for cmd in [
        "git checkout -- src/main.rs",
        "git checkout -- Cargo.toml",
        "git checkout -- Cargo.lock",
        "git checkout HEAD~1 -- src/main.rs",
        "git checkout HEAD -- README.md",
    ] {
        assert_eq!(bash(cmd), Decision::Allow, "expected Allow for `{cmd}`");
    }
    // The disarm cases the narrowing must NOT reopen: a checkout of a PROTECTED
    // pathspec still discards its uncommitted protected edits, and the
    // whole-tree form sweeps protected paths in implicitly.
    for cmd in [
        "git checkout -- .claude/settings.json",
        "git checkout -- .githooks/pre-commit",
        "git checkout HEAD -- .githooks/pre-commit",
        "git checkout -- deny.toml",
        "git checkout -- .",
        "git checkout --force",
    ] {
        assert!(bash(cmd).is_deny(), "expected Deny for `{cmd}`");
    }
}

#[test]
fn git_switch_discard_forms_are_denied() {
    for cmd in [
        "git switch -f main",
        "git switch --force main",
        "git switch --discard-changes main",
    ] {
        assert!(bash(cmd).is_deny(), "expected Deny for `{cmd}`");
    }
    assert_eq!(bash("git switch main"), Decision::Allow);
    assert_eq!(bash("git switch -c feature/x"), Decision::Allow);
}

#[test]
fn git_clean_force_without_d_or_x_is_denied() {
    assert!(bash("git clean -f").is_deny());
    assert!(bash("git clean --force").is_deny());
    assert!(bash("git clean -fd").is_deny());
    // `-n`/`--dry-run` carries no `f`, so the new arm cannot fire on it.
    assert!(!bash("git clean -n").is_deny());
}

#[test]
fn find_exec_mv_placeholder_blocks() {
    assert_blocking(
        bash("find . -name pre-commit -exec mv {} /tmp/ \\;"),
        "find -exec mv {}",
    );
    assert_blocking(
        bash("find . -name pre-commit -exec mv {} /tmp/ +"),
        "find -exec mv {} +",
    );
    // Control: the already-denied twin.
    assert!(bash("find . -name pre-commit -exec rm {} \\;").is_deny());
    // Control: a read-only exec is untouched.
    assert_eq!(
        bash("find . -name '*.md' -exec grep -l rm {} \\;"),
        Decision::Allow
    );
}
