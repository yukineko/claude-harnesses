// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Probes for the verdict paths a mechanical census flagged as CANDIDATES.
//!
//! A candidate found by READING is a prediction. CLAUDE.md 2 says a prediction
//! is settled by observation before it is called a finding — or dropped. These
//! are that step, kept afterwards because the ones that were dropped are worth
//! as much as the one that was not: without them, the same three suspicions get
//! re-raised by the next reader of the same code.
//!
//! The census listed 120 permissive-or-collapsing terminals in this crate's
//! production code (method and full table in
//! `docs/audit-blastguard-verdict-paths.md`). Most are "checked, found no
//! hazard" — a determination, not a failure to make one. Three looked like
//! "could not determine, answered fine". Measured 2026-08-02 against 0.2.38:
//!
//!   1. REFUTED — `unknown_wrapper_ask` requires EXACTLY ONE command candidate
//!      and `command_candidates` fans out behind an exec wrapper, so a wrapped
//!      line looked like it could fall out of the gate. It does not: recognised
//!      wrappers are handled by `analyze_command_at` before this rule is
//!      reached. `sudo`/`env`/`nohup`/`timeout`/`nice`/`stdbuf`/`command` in
//!      front of `rm -rf /` all still deny (PROBE 1).
//!   2. REFUTED — `analyze_xargs` answers `Allow` when `xargs_command_start`
//!      returns `None`. Eleven `xargs` spellings including `-I{}`, `-I {}`,
//!      `-J %`, `--replace={}` and a trailing `--` all still reach the inner
//!      command (PROBE 3).
//!   3. CONFIRMED, and not the one predicted — the real defect was in the same
//!      function but on the RESULT rather than the operand: a nested `Ask` was
//!      discarded because the recursion tested `.is_deny()`, so TWO unknown
//!      verbs in front of `rm -rf /` answered `Allow` while ONE correctly asked
//!      (PROBE 2b). Fixed in 0.2.39.
//!
//! A fourth, the `chmod`/`chown` mirror, is RECORDED not asserted — see
//! `probe_records_the_chmod_chown_asymmetry` for why measuring it is a question
//! about the OS rather than about this crate.
//!
//! Each probe states what it would mean if it FAILED, so a green run is
//! informative rather than decorative.

use blastguard::detect::detect;
use serde_json::json;

fn bash(cmd: &str) -> blastguard::model::Decision {
    detect("Bash", Some(&json!({ "command": cmd })))
}

/// PROBE 1 — is a multi-candidate exec wrapper analysed, or dropped to Allow?
///
/// `sudo`/`env`/`timeout` are RECOGNISED wrappers, so `analyze_command_at`
/// should descend through them regardless of what `unknown_wrapper_ask` does.
/// If any of these is `Allow`, the wrapper path leaks a destructive payload and
/// the census candidate is a real fail-open.
#[test]
fn probe_multi_candidate_exec_wrapper_is_not_dropped_to_allow() {
    for cmd in [
        "sudo rm -rf /",
        "env FOO=1 rm -rf /",
        "nohup rm -rf / &",
        "timeout 5 rm -rf /",
        "nice -n 10 rm -rf /",
        "stdbuf -o0 rm -rf /",
        "command rm -rf /",
        "env FOO=1 BAR=2 rm -rf /",
    ] {
        let d = bash(cmd);
        assert!(
            !matches!(d, blastguard::model::Decision::Allow),
            "wrapper path allowed a destructive payload: {cmd:?} -> {d:?}"
        );
    }
}

/// PROBE 2 — ONE unrecognised verb in front of a destructive line Asks.
///
/// This is the behaviour `unknown_wrapper_ask` was built for, and it works.
/// Kept as the control for the nesting test below: if this ever regresses, the
/// nesting result would be meaningless.
#[test]
fn one_unknown_wrapper_with_a_destructive_tail_is_not_allowed() {
    for cmd in [
        "myrunner rm -rf /",
        "./scripts/wrap.sh rm -rf /",
        "uvx rm -rf /",
        "npx rm -rf /",
        "dlx rm -rf /",
    ] {
        let d = bash(cmd);
        assert!(
            !matches!(d, blastguard::model::Decision::Allow),
            "one unknown wrapper allowed a destructive tail: {cmd:?} -> {d:?}"
        );
    }
}

/// PROBE 2b — TWO unrecognised verbs must not be SAFER than one.
///
/// Measured 2026-08-02, blastguard 0.2.38, before the fix:
///
/// ```text
///   dlx rm -rf /                   -> ask
///   pnpm dlx rm -rf /              -> ALLOW
///   cargo run rm -rf /             -> ALLOW
///   docker run rm -rf /            -> ALLOW
///   myrunner myrunner rm -rf /     -> ALLOW
///   a b rm -rf /                   -> ALLOW
///   a b c rm -rf /                 -> ALLOW
/// ```
///
/// `unknown_wrapper_ask` re-analyses the tail and raises an `Ask` only when the
/// tail is a DENY (`detect.rs:2520`, `if detect_bash(&tail, depth + 1).is_deny()`).
/// One level down, `dlx rm -rf /` is itself an `Ask` — not a `Deny` — so the
/// test was false and the function fell through to its `Decision::Allow` tail.
///
/// The nested `Ask` is not a weaker signal than a `Deny`; it is the SAME
/// statement blastguard makes at the top level, one frame down: "there is a
/// destructive command line here and I cannot tell whether this verb runs it."
/// Discarding it turns "could not determine" into "fine", which is the defect
/// CLAUDE.md 3 names, reached through this crate's own recursion. Its practical
/// effect is that prefixing ANY unrecognised token defeats the unknown-wrapper
/// rule outright.
///
/// Only THIS Ask class is forwarded. The others still collapse to `Allow` in
/// that frame — see `residual_nested_ask_classes_still_collapse_to_allow`.
#[test]
fn two_unknown_wrappers_are_not_safer_than_one() {
    for cmd in [
        "pnpm dlx rm -rf /",
        "cargo run rm -rf /",
        "docker run rm -rf /",
        "myrunner myrunner rm -rf /",
        "a b rm -rf /",
        "a b c rm -rf /",
        "pnpm run truncate -s 0 important.db",
    ] {
        let d = bash(cmd);
        assert!(
            !matches!(d, blastguard::model::Decision::Allow),
            "nesting defeated the unknown-wrapper rule: {cmd:?} -> {d:?}"
        );
    }
}

/// The residual, pinned so it is a KNOWN permissive set rather than a cleared
/// one.
///
/// `unknown_wrapper_ask` forwards only the unknown-verb Ask. The other nested
/// Ask classes still collapse to `Allow` in that frame. This test asserts that
/// residual rather than describing it, so the day it is closed this test fails
/// and the claim gets updated instead of quietly rotting.
///
/// Why it was not closed here: forwarding every nested Ask was tried and
/// over-blocked two measured benign cases — `echo $(date 2>&1)` (the
/// unresolvable-expansion Ask, raised because the analyser reads `$(date` in
/// tail position as a command word) and `echo hi` inside 7 quote-only `sh -c`
/// layers (the depth-exhaustion Ask, which
/// `tests/backslash_escape_nesting_fail_open.rs` pins as ALLOW on the grounds
/// that blocking there gates on the over-escaped SHAPE, not on any destructive
/// word). Closing it needs the expansion Ask to tell command position from
/// argument position, which is separate work.
///
/// Neither case is a regression from this change: the old `.is_deny()` test
/// dropped all of them too.
#[test]
fn residual_nested_ask_classes_still_collapse_to_allow() {
    // Depth exhaustion behind an unknown verb: 16 nested wrappers.
    let deep: String = (0..16).map(|i| format!("w{i} ")).collect::<String>() + "rm -rf /";
    assert!(
        matches!(bash(&deep), blastguard::model::Decision::Allow),
        "residual changed: deep nesting now answers {:?}",
        bash(&deep)
    );

    // The unresolvable-expansion Ask behind a verb blastguard does not know.
    assert!(
        matches!(
            bash("echo $(date 2>&1)"),
            blastguard::model::Decision::Allow
        ),
        "residual changed: expansion-in-tail now answers {:?}",
        bash("echo $(date 2>&1)")
    );
}

/// ANTI-VACUITY CONTROL for the nesting fix.
///
/// Propagating a nested `Ask` must not turn every multi-word command into a
/// question. These all have an ordinary, non-destructive tail, so the nested
/// analysis returns `Allow` and there is nothing to propagate. If the fix were
/// "Ask whenever the tail is not provably clean", this test fails.
#[test]
fn ordinary_nested_commands_still_allow() {
    for cmd in [
        "a b ls",
        "pnpm run build",
        "cargo run --release",
        "docker run -it ubuntu bash",
        "myrunner myrunner echo hello",
        "npm run test -- --watch",
        "uv run python script.py",
    ] {
        let d = bash(cmd);
        assert!(
            matches!(d, blastguard::model::Decision::Allow),
            "ordinary nested command stopped allowing: {cmd:?} -> {d:?}"
        );
    }
}

/// PROBE 3 — `xargs` forms whose command start the parser may not locate.
///
/// `xargs_command_start` returns `None` for a trailing `--` with nothing after
/// it, and walks a flag table to find the first non-flag token. A form that
/// slips past that table would carry `rm -rf /` into `Allow`.
#[test]
fn probe_xargs_forms_do_not_hide_a_destructive_inner_command() {
    for cmd in [
        "xargs rm -rf /",
        "xargs -I{} rm -rf /",
        "xargs -I {} rm -rf /",
        "xargs -n1 rm -rf /",
        "xargs -n 1 rm -rf /",
        "xargs -J % rm -rf /",
        "xargs -- rm -rf /",
        "xargs --replace={} rm -rf /",
        "xargs -0 rm -rf /",
        "xargs -P4 -n1 rm -rf /",
        "echo x | xargs -t rm -rf /",
    ] {
        let d = bash(cmd);
        assert!(
            !matches!(d, blastguard::model::Decision::Allow),
            "xargs form hid a destructive inner command: {cmd:?} -> {d:?}"
        );
    }
}

/// PROBE 4 — the chmod/chown mirror.
///
/// Non-recursive `chmod` onto a protected gate path is denied because removing
/// read or exec DISARMS the hook without writing a byte of it. `chown`'s
/// non-recursive arm is an unconditional `Allow`. This probe RECORDS which way
/// that goes; it deliberately does not assert the chown case, because whether
/// chown can disarm anything here is a question about the OS, not about
/// blastguard, and asserting a `Deny` I have not shown to be warranted would be
/// the same unbacked claim in the other direction.
#[test]
fn probe_records_the_chmod_chown_asymmetry() {
    let chmod = bash("chmod 000 .githooks/pre-commit");
    let chown = bash("chown nobody .githooks/pre-commit");
    let chgrp = bash("chown :staff .githooks/pre-commit");

    // The chmod half is settled behaviour and is asserted, so this test fails
    // if the disarm rule ever regresses.
    assert!(
        !matches!(chmod, blastguard::model::Decision::Allow),
        "chmod disarm rule regressed: {chmod:?}"
    );

    // The chown half is only recorded. `cargo test -- --nocapture` prints it.
    println!("chown protected target  -> {chown:?}");
    println!("chown :group protected  -> {chgrp:?}");
}
