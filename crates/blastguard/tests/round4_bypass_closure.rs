// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Round 4 of the adversarial review of the protected-path rule (blastguard
//! 0.2.20 -> 0.2.21). Every DENY/ASK scenario here was OBSERVED to be `Allow`
//! on the shipped 0.2.20 binary before this file existed; each one disarms a
//! protected gate/hook/policy path through a spelling no rule classified:
//!
//!   1&2. `sort -o <protected>` / `uniq <in> <protected>` — two entries of the
//!        "READ_ONLY" exemption have OUTPUT-writing forms and were waved through
//!        without inspecting operands (proven: truncated a real file to 0 bytes).
//!   3.   `chmod -r <hook>` / `chmod 311 <hook>` — removing the READ bit (not
//!        just exec) disarms a shell hook: the interpreter cannot read the script
//!        to run it (proven on this host: exit 126 "Permission denied").
//!   4.   `rm -d <protected-container>` — `-d`/`--dir` removes an EMPTY protected
//!        directory and was not gated by the `if recursive` container check.
//!
//! Written as an integration test on purpose: the findings are about the
//! END-TO-END verdict a tool call receives, not any one helper. `detect::detect`
//! is the same entry point `main.rs` uses.
//!
//! Every group carries BOTH controls — a known-DENY and a known-ALLOW — because
//! an earlier round produced a false all-clear from a stale binary: with no
//! control, "everything returned Allow" is indistinguishable from "the probe
//! never reached the code under test".

use blastguard::detect::detect;
use blastguard::model::Decision;
use serde_json::json;

fn bash(cmd: &str) -> Decision {
    detect("Bash", Some(&json!({ "command": cmd })))
}

/// A blocking verdict is Deny OR Ask: both stop the call from running
/// unreviewed, and `Decision::hardened` collapses Ask to Deny wherever no human
/// is present.
fn assert_blocking(d: Decision, what: &str) {
    assert!(
        d.is_blocking(),
        "expected a blocking verdict for `{what}`, got {d:?}"
    );
}

fn assert_allow(d: Decision, what: &str) {
    assert!(
        matches!(d, Decision::Allow),
        "expected Allow for `{what}`, got {d:?}"
    );
}

// ---------------------------------------------------------------------------
// Controls. If these ever fail, no other assertion in this file means anything.
// ---------------------------------------------------------------------------

#[test]
fn controls() {
    // Known DENY.
    assert!(bash("rm -rf .githooks").is_deny());
    // Known ALLOW.
    assert_allow(bash("ls -la"), "ls -la");
}

// ---------------------------------------------------------------------------
// Findings 1 & 2 — sort -o / uniq output writing to a protected path.
// ---------------------------------------------------------------------------

#[test]
fn sort_output_onto_protected_is_blocked() {
    // `sort -o FILE` writes FILE in place (proven: truncated a real file).
    assert_blocking(
        bash("sort -o .githooks/pre-commit /dev/null"),
        "sort -o .githooks/pre-commit /dev/null",
    );
    // Attached short form.
    assert_blocking(
        bash("sort -o.githooks/pre-commit /dev/null"),
        "sort -o.githooks/pre-commit",
    );
    // Long form.
    assert_blocking(
        bash("sort --output=.githooks/pre-commit /dev/null"),
        "sort --output=.githooks/pre-commit",
    );
    // Separate long form.
    assert_blocking(
        bash("sort --output .claude/settings.json /dev/null"),
        "sort --output .claude/settings.json",
    );
    // Clustered short flags where -o is last (`-b -o`).
    assert_blocking(
        bash("sort -bo .githooks/pre-commit /dev/null"),
        "sort -bo .githooks/pre-commit",
    );
}

#[test]
fn uniq_output_operand_onto_protected_is_blocked() {
    // `uniq [INPUT [OUTPUT]]` — the 2nd positional operand is written.
    assert_blocking(
        bash("uniq /dev/null .githooks/pre-commit"),
        "uniq /dev/null .githooks/pre-commit",
    );
    assert_blocking(
        bash("uniq input.txt .claude/settings.json"),
        "uniq input.txt .claude/settings.json",
    );
}

#[test]
fn sort_uniq_ordinary_use_stays_allow() {
    // Pure reads and non-protected outputs must not be over-blocked.
    assert_allow(bash("sort data.txt"), "sort data.txt");
    assert_allow(bash("sort -o out.txt in.txt"), "sort -o out.txt in.txt");
    assert_allow(bash("sort -r data.txt"), "sort -r data.txt");
    assert_allow(bash("uniq a.txt b.txt"), "uniq a.txt b.txt");
    assert_allow(bash("uniq input.txt"), "uniq input.txt");
    assert_allow(bash("uniq -c input.txt"), "uniq -c input.txt");
    // Reading a protected file (no output form) stays Allow — sort/uniq read
    // semantics are known, unlike an unknown verb.
    assert_allow(bash("sort .githooks/pre-commit"), "sort <protected> (read)");
    assert_allow(
        bash("uniq .githooks/pre-commit"),
        "uniq <protected> (single-operand read)",
    );
}

// ---------------------------------------------------------------------------
// Finding 3 — chmod that removes the READ bit disarms a shell hook.
// ---------------------------------------------------------------------------

#[test]
fn chmod_removing_read_from_protected_hook_is_blocked() {
    // Symbolic: subtract read.
    assert_blocking(
        bash("chmod -r .githooks/pre-commit"),
        "chmod -r .githooks/pre-commit",
    );
    assert_blocking(
        bash("chmod a-r .githooks/pre-commit"),
        "chmod a-r .githooks/pre-commit",
    );
    assert_blocking(
        bash("chmod u-r .githooks/pre-commit"),
        "chmod u-r .githooks/pre-commit",
    );
    // Octal: owner triad with the read bit (4) clear but exec set (311, 100).
    assert_blocking(
        bash("chmod 311 .githooks/pre-commit"),
        "chmod 311 .githooks/pre-commit",
    );
    assert_blocking(
        bash("chmod 100 .githooks/pre-commit"),
        "chmod 100 .githooks/pre-commit",
    );
    // `=` replace that drops read.
    assert_blocking(
        bash("chmod a=x .githooks/pre-commit"),
        "chmod a=x .githooks/pre-commit",
    );
}

#[test]
fn chmod_that_keeps_read_and_exec_or_targets_ordinary_files_stays_allow() {
    // Installing / re-arming a hook must NOT be blocked.
    assert_allow(
        bash("chmod +x .githooks/pre-commit"),
        "chmod +x .githooks/pre-commit",
    );
    assert_allow(
        bash("chmod 755 .githooks/pre-commit"),
        "chmod 755 .githooks/pre-commit",
    );
    assert_allow(
        bash("chmod 700 .githooks/pre-commit"),
        "chmod 700 .githooks/pre-commit",
    );
    // Ordinary non-protected targets stay Allow regardless of mode.
    assert_allow(bash("chmod 644 README.md"), "chmod 644 README.md");
    assert_allow(bash("chmod -r README.md"), "chmod -r README.md");
    assert_allow(bash("chmod +x scripts/run.sh"), "chmod +x scripts/run.sh");
}

// ---------------------------------------------------------------------------
// Finding 4 — rm -d / --dir on a protected container.
// ---------------------------------------------------------------------------

#[test]
fn rm_dir_flag_on_protected_container_is_denied() {
    assert!(
        bash("rm -d .claude/hooks").is_deny(),
        "rm -d .claude/hooks should Deny"
    );
    assert!(
        bash("rm --dir .git/hooks").is_deny(),
        "rm --dir .git/hooks should Deny"
    );
    assert!(bash("rm -d .claude").is_deny(), "rm -d .claude should Deny");
    // Combined with -f is the same container removal.
    assert!(
        bash("rm -df .githooks").is_deny(),
        "rm -df .githooks should Deny"
    );
}

#[test]
fn rm_dir_flag_on_ordinary_path_stays_allow() {
    assert_allow(bash("rm -d emptydir"), "rm -d emptydir");
    assert_allow(bash("rm --dir build/stale"), "rm --dir build/stale");
}
