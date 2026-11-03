// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Evidence ledger contract tests (`fde evidence red|green|pass|verify`, `fde report`).
//!
//! Written by an independent test author (CLAUDE.md §2(a)); the implementer
//! must match the contract, not edit these tests to pass.
//!
//! Every test drives the built binary against a real temp git repo, with the
//! state dir (`FDE_STATE_DIR`) and the spec file in separate temp dirs outside
//! the target. Ratified state is reached through `fde begin` + `fde ratify`.
//!
//! Commands given via `--cmd` are deliberately free of shell operators
//! (`true`, `false`, `test -f X`, `test ! -f X`) so the tests do not depend on
//! whether the implementation runs them through `sh -c` or splits words.
//!
//! Refusal cases never rely on "non-zero" alone: they either pin the exact
//! exit code of a determined answer (1) or check the side effect (no record
//! was written: a later GREEN that needs that RED is refused, verify stays
//! incomplete), so a stub exiting 2 everywhere does not satisfy them.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use common::{changed_files, code, show, snapshot_files, stderr, stdout, Engagement};

/// `fde evidence KIND --req REQ --cmd CMD`, run with cwd `cwd`.
fn ev(e: &Engagement, cwd: &Path, kind: &str, req: &str, cmd: &str) -> Output {
    e.fde_in(cwd, &["evidence", kind, "--req", req, "--cmd", cmd])
}

/// Same as [`ev`] with cwd = target root.
fn ev_root(e: &Engagement, kind: &str, req: &str, cmd: &str) -> Output {
    ev(e, e.root(), kind, req, cmd)
}

fn assert_code(out: &Output, want: i32, what: &str) {
    assert_eq!(code(out), Some(want), "{what}: {}", show(out));
}

fn verify(e: &Engagement) -> Output {
    e.fde(&["evidence", "verify"])
}

/// Complete `req` with RED then GREEN (`test -f <flag>` at the target root).
/// Used where a test needs a complete requirement: user ruling 2026-11-04:
/// PASS-only is not complete.
fn complete(e: &Engagement, req: &str, flag: &str) {
    let cmd = format!("test -f {flag}");
    assert_code(&ev_root(e, "red", req, &cmd), 0, &format!("RED {req}"));
    fs::write(e.root().join(flag), "").unwrap();
    assert_code(&ev_root(e, "green", req, &cmd), 0, &format!("GREEN {req}"));
}

/// Phrases that would read as "everything is fine" to a human or a script.
const ALL_CLEAR: &[&str] = &[
    "all clear",
    "all-clear",
    "all requirements satisfied",
    "all requirements met",
    "all requirements verified",
    "all green",
    "no unresolved",
    "nothing unresolved",
    "evidence complete",
    "everything",
];

fn assert_no_all_clear(text: &str, ctx: &str) {
    let low = text.to_lowercase();
    for p in ALL_CLEAR {
        assert!(
            !low.contains(p),
            "{ctx}: output must not contain an all-clear line ({p:?}):\n{text}"
        );
    }
}

/// Lines of `text` that mention requirement `id` as a whole token.
fn lines_mentioning<'a>(text: &'a str, id: &str) -> Vec<&'a str> {
    text.lines()
        .filter(|l| {
            l.split(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
                .any(|tok| tok == id)
        })
        .collect()
}

/// Record a RED for R1 and return the state-dir files that recording created
/// or changed (the ledger, as far as the test can see it). Asserts there is at least one.
fn ledger_files_after_red(e: &Engagement) -> Vec<PathBuf> {
    let before = snapshot_files(&e.state.path);
    let out = ev_root(e, "red", "R1", "false");
    assert_code(&out, 0, "RED with a failing command must be recorded");
    let after = snapshot_files(&e.state.path);
    let changed = changed_files(&e.state.path, &before, &after);
    assert!(
        !changed.is_empty(),
        "recording RED must write the ledger into FDE_STATE_DIR ({})",
        e.state.path.display()
    );
    // Control before the caller damages the ledger: the intact ledger gives a
    // determined answer, so a later 2 comes from the damage, not from a
    // verify that always answers 2.
    assert_code(&verify(e), 1, "intact ledger with RED only is incomplete");
    changed
}

// ---------------------------------------------------------------------------
// (1) RED
// ---------------------------------------------------------------------------

#[test]
fn red_is_recorded_when_command_fails() {
    let e = Engagement::ratified(&[("R1", "first")]);
    // `test -f fixed` fails now (no such file) ...
    assert_code(
        &ev_root(&e, "red", "R1", "test -f fixed"),
        0,
        "RED with a failing command must be recorded",
    );
    // ... the RED alone leaves the requirement incomplete (determined, not undetermined).
    assert_code(&verify(&e), 1, "RED without GREEN is incomplete");
    // Proof the RED was actually recorded: the matching GREEN is now accepted.
    fs::write(e.root().join("fixed"), "").unwrap();
    assert_code(
        &ev_root(&e, "green", "R1", "test -f fixed"),
        0,
        "GREEN after a recorded RED (same req, same cmd) must be recorded",
    );
    assert_code(&verify(&e), 0, "RED then GREEN is complete");
}

#[test]
fn red_is_rejected_when_command_passes() {
    let e = Engagement::ratified(&[("R1", "first")]);
    let out = ev_root(&e, "red", "R1", "true");
    assert_code(
        &out,
        1,
        "a passing command contradicts RED (determined rejection)",
    );
    assert!(
        !stderr(&out).trim().is_empty(),
        "rejection must state a reason"
    );
    // No RED was recorded: a GREEN for the same req+cmd has nothing to follow.
    assert_code(
        &ev_root(&e, "green", "R1", "true"),
        1,
        "GREEN without a recorded RED must be refused",
    );
    assert_code(&verify(&e), 1, "rejected RED leaves R1 incomplete");
    // Positive control: the rejection is specific to the passing command, so
    // a binary that answers 1 to everything does not satisfy this test.
    assert_code(
        &ev_root(&e, "red", "R1", "false"),
        0,
        "RED with a failing command must still be recorded",
    );
}

// ---------------------------------------------------------------------------
// (2) GREEN
// ---------------------------------------------------------------------------

#[test]
fn green_requires_prior_red_for_same_req_and_same_cmd() {
    let e = Engagement::ratified(&[("R1", "first"), ("R2", "second")]);
    assert_code(
        &ev_root(&e, "red", "R1", "test -f fixed"),
        0,
        "RED for R1 must be recorded",
    );
    fs::write(e.root().join("fixed"), "").unwrap();

    // Same cmd, different req: R2 has no RED.
    assert_code(
        &ev_root(&e, "green", "R2", "test -f fixed"),
        1,
        "GREEN for R2 must not borrow R1's RED",
    );
    // Same req, different cmd (semantically equivalent, textually different).
    assert_code(
        &ev_root(&e, "green", "R1", "test -e fixed"),
        1,
        "GREEN must use the same cmd as the RED",
    );
    // The matching pair is accepted.
    assert_code(
        &ev_root(&e, "green", "R1", "test -f fixed"),
        0,
        "GREEN for the same req and cmd must be recorded",
    );
    // R2 still lacks evidence, and verify names it.
    let v = verify(&e);
    assert_code(&v, 1, "R2 has no evidence yet");
    assert!(
        stderr(&v).contains("R2"),
        "incomplete verify must name the missing requirement: {}",
        show(&v)
    );
    // Positive control: completing R2 makes verify pass.
    complete(&e, "R2", "fixed2");
    assert_code(&verify(&e), 0, "R1 and R2 both RED+GREEN is complete");
}

#[test]
fn green_without_any_red_is_rejected() {
    let e = Engagement::ratified(&[("R1", "first")]);
    let out = ev_root(&e, "green", "R1", "true");
    assert_code(&out, 1, "GREEN with no prior RED must be refused");
    assert!(
        !stderr(&out).trim().is_empty(),
        "refusal must state a reason"
    );
    assert_code(&verify(&e), 1, "refused GREEN leaves R1 incomplete");
    // Positive control: R1 can be completed, so the 1s above are not a
    // constant answer.
    // user ruling 2026-11-04: PASS-only is not complete
    assert_code(&ev_root(&e, "pass", "R1", "true"), 0, "PASS R1");
    assert_code(&verify(&e), 1, "R1 PASS-only is not complete");
    complete(&e, "R1", "fixed-r1");
    assert_code(&verify(&e), 0, "PASS then RED then GREEN is complete");
}

#[test]
fn green_is_rejected_when_command_fails() {
    let e = Engagement::ratified(&[("R1", "first")]);
    assert_code(
        &ev_root(&e, "red", "R1", "false"),
        0,
        "RED with a failing command must be recorded",
    );
    assert_code(
        &ev_root(&e, "green", "R1", "false"),
        1,
        "a failing command contradicts GREEN",
    );
    assert_code(&verify(&e), 1, "rejected GREEN leaves R1 incomplete");
}

// ---------------------------------------------------------------------------
// (3) PASS (conforming to the current state)
// ---------------------------------------------------------------------------

#[test]
fn pass_records_already_passing_requirement_separately_from_red_green() {
    let e = Engagement::ratified(&[("R1", "first")]);
    assert_code(
        &ev_root(&e, "pass", "R1", "true"),
        0,
        "PASS with a passing command must be recorded",
    );
    // user ruling 2026-11-04: PASS-only is not complete
    assert_code(&verify(&e), 1, "a PASS-only record does not complete R1");

    // PASS is not a RED: it does not license a GREEN.
    assert_code(
        &ev_root(&e, "green", "R1", "true"),
        1,
        "a PASS record must not count as the RED a GREEN needs",
    );

    // The report shows it as conforming/pass, not as GREEN.
    // PASS-only is not "verified RED then GREEN": the report is printed but
    // exits 1 (determined not done) with a reason.
    let r = e.fde(&["report"]);
    assert_code(&r, 1, "report on a PASS-only requirement");
    assert!(
        !stderr(&r).trim().is_empty(),
        "report exit 1 must state a reason: {}",
        show(&r)
    );
    let text = stdout(&r);
    let lines = lines_mentioning(&text, "R1");
    assert!(!lines.is_empty(), "report must list R1:\n{text}");
    assert!(
        lines.iter().any(|l| {
            let low = l.to_lowercase();
            low.contains("pass") || low.contains("conform")
        }),
        "report must mark R1 as pass/conforming:\n{text}"
    );
    assert!(
        lines.iter().all(|l| !l.contains("GREEN")),
        "a PASS record must not be reported as GREEN:\n{text}"
    );
}

#[test]
fn pass_is_rejected_when_command_fails() {
    let e = Engagement::ratified(&[("R1", "first")]);
    let out = ev_root(&e, "pass", "R1", "false");
    assert_code(&out, 1, "a failing command contradicts PASS");
    assert!(
        !stderr(&out).trim().is_empty(),
        "rejection must state a reason"
    );
    assert_code(&verify(&e), 1, "rejected PASS leaves R1 incomplete");
    // Positive control: a passing command is accepted and completes R1, so
    // the 1s above are not a constant answer.
    // user ruling 2026-11-04: PASS-only is not complete
    assert_code(&ev_root(&e, "pass", "R1", "true"), 0, "PASS R1");
    assert_code(&verify(&e), 1, "R1 PASS-only is not complete");
    complete(&e, "R1", "fixed-r1");
    assert_code(&verify(&e), 0, "PASS then RED then GREEN is complete");
}

// ---------------------------------------------------------------------------
// (4) verify
// ---------------------------------------------------------------------------

#[test]
fn verify_with_no_evidence_is_incomplete() {
    let e = Engagement::ratified(&[("R1", "first"), ("R2", "second")]);
    let v = verify(&e);
    assert_code(&v, 1, "no evidence at all is a determined incomplete");
    assert!(
        !stderr(&v).trim().is_empty(),
        "incomplete verify must state a reason"
    );
    // Positive control: with evidence for both, the same verify answers 0.
    // user ruling 2026-11-04: PASS-only is not complete
    complete(&e, "R1", "fixed1");
    complete(&e, "R2", "fixed2");
    assert_code(&verify(&e), 0, "every requirement has RED+GREEN");
}

#[test]
fn verify_red_only_is_incomplete_and_completion_flips_it() {
    let e = Engagement::ratified(&[("R1", "first"), ("R2", "second")]);
    // user ruling 2026-11-04: PASS-only is not complete
    complete(&e, "R2", "fixed2");
    assert_code(&ev_root(&e, "red", "R1", "test -f fixed"), 0, "RED R1");
    let v = verify(&e);
    assert_code(&v, 1, "R1 has RED but no GREEN");
    assert!(
        stderr(&v).contains("R1"),
        "incomplete verify must name R1: {}",
        show(&v)
    );
    fs::write(e.root().join("fixed"), "").unwrap();
    assert_code(&ev_root(&e, "green", "R1", "test -f fixed"), 0, "GREEN R1");
    assert_code(&verify(&e), 0, "every requirement now has evidence");
}

#[test]
fn verify_corrupt_ledger_is_undetermined() {
    let e = Engagement::ratified(&[("R1", "first")]);
    let ledger = ledger_files_after_red(&e);
    for f in &ledger {
        fs::write(f, b"\x00\xffnot a ledger {{{[[[").unwrap();
    }
    let v = verify(&e);
    assert_code(&v, 2, "a corrupt ledger is undetermined, not incomplete");
    assert!(
        !stderr(&v).trim().is_empty(),
        "undetermined verify must state a reason"
    );
}

#[test]
fn verify_unreadable_ledger_is_undetermined() {
    use std::os::unix::fs::PermissionsExt;
    let e = Engagement::ratified(&[("R1", "first")]);
    let ledger = ledger_files_after_red(&e);
    for f in &ledger {
        fs::set_permissions(f, fs::Permissions::from_mode(0o000)).unwrap();
    }
    if ledger.iter().any(|f| fs::read(f).is_ok()) {
        // Running as a user that ignores permission bits (root): the
        // unreadable condition cannot be constructed here. Restore and fail
        // loudly rather than pass silently.
        for f in &ledger {
            let _ = fs::set_permissions(f, fs::Permissions::from_mode(0o600));
        }
        panic!("cannot make the ledger unreadable in this environment (running as root?)");
    }
    let v = verify(&e);
    for f in &ledger {
        fs::set_permissions(f, fs::Permissions::from_mode(0o600)).unwrap();
    }
    assert_code(
        &v,
        2,
        "an unreadable ledger is undetermined, not incomplete",
    );
}

#[test]
fn verify_spec_with_zero_requirements_is_undetermined() {
    let e = Engagement::ratified(&[]);
    let v = verify(&e);
    assert_code(
        &v,
        2,
        "zero ratified requirements is undetermined, never complete",
    );
    assert!(
        !stderr(&v).trim().is_empty(),
        "undetermined verify must state a reason"
    );
    // Control: the same fixture with one requirement and a PASS gives 0, so
    // the 2 above can only come from the zero-requirement spec, not from a
    // verify that always answers 2.
    let one = Engagement::ratified(&[("R1", "first")]);
    // user ruling 2026-11-04: PASS-only is not complete
    complete(&one, "R1", "fixed1");
    assert_code(
        &verify(&one),
        0,
        "one requirement with RED+GREEN is complete",
    );
}

// ---------------------------------------------------------------------------
// (5) unratified targets are refused
// ---------------------------------------------------------------------------

#[test]
fn evidence_is_refused_for_registered_but_unratified_target() {
    let e = Engagement::new(&[("R1", "first")]);
    e.begin();
    let red = ev_root(&e, "red", "R1", "false");
    assert_ne!(
        code(&red),
        Some(0),
        "RED on an unratified target: {}",
        show(&red)
    );
    assert!(
        !stderr(&red).trim().is_empty(),
        "refusal must state a reason"
    );
    let pass = ev_root(&e, "pass", "R1", "true");
    assert_ne!(
        code(&pass),
        Some(0),
        "PASS on an unratified target: {}",
        show(&pass)
    );
    assert!(
        !stderr(&pass).trim().is_empty(),
        "refusal must state a reason"
    );

    // After ratification, nothing from the refused attempts was kept.
    e.ratify();
    assert_code(
        &ev_root(&e, "green", "R1", "true"),
        1,
        "the refused RED must not have been recorded",
    );
    assert_code(
        &verify(&e),
        1,
        "the refused PASS must not have been recorded",
    );
    // Positive control: once ratified, the same RED is accepted, so the
    // refusals above are about ratification, not a constant answer.
    assert_code(
        &ev_root(&e, "red", "R1", "false"),
        0,
        "RED on the ratified target must be recorded",
    );
}

#[test]
fn evidence_is_refused_for_unregistered_target() {
    let e = Engagement::new(&[("R1", "first")]);
    let red = ev_root(&e, "red", "R1", "false");
    assert_ne!(
        code(&red),
        Some(0),
        "RED on an unregistered target: {}",
        show(&red)
    );
    assert!(
        !stderr(&red).trim().is_empty(),
        "refusal must state a reason"
    );
    e.begin();
    e.ratify();
    assert_code(
        &ev_root(&e, "green", "R1", "true"),
        1,
        "the refused RED must not have been recorded",
    );
    // Positive control: once registered and ratified, the same RED is accepted.
    assert_code(
        &ev_root(&e, "red", "R1", "false"),
        0,
        "RED on the ratified target must be recorded",
    );
}

#[test]
fn evidence_is_refused_after_spec_changed_since_ratification() {
    let e = Engagement::ratified(&[("R1", "first")]);
    common::write_spec(
        &e.spec_dir.path,
        "spec.toml",
        "fixture",
        &[("R1", "first"), ("R2", "added after ratification")],
    );
    assert_code(&e.fde(&["status"]), 1, "a changed spec is unratified");
    // `test -f fixed` fails now, so a RED would be recorded if it were accepted.
    let red = ev_root(&e, "red", "R1", "test -f fixed");
    assert_ne!(code(&red), Some(0), "RED after spec change: {}", show(&red));
    assert!(
        !stderr(&red).trim().is_empty(),
        "refusal must state a reason"
    );

    // Restore the ratified bytes: the fingerprint matches again, so the
    // target is ratified without a new ratification.
    common::write_spec(&e.spec_dir.path, "spec.toml", "fixture", &[("R1", "first")]);
    assert_code(
        &e.fde(&["status"]),
        0,
        "restored spec matches the ratification",
    );
    // Side effect: nothing was recorded by the refused RED, so the GREEN that
    // would need it is refused (determined, exit 1).
    fs::write(e.root().join("fixed"), "").unwrap();
    assert_code(
        &ev_root(&e, "green", "R1", "test -f fixed"),
        1,
        "the RED refused after the spec change must not have been recorded",
    );
    // Positive control: on the ratified target a RED is accepted.
    assert_code(
        &ev_root(&e, "red", "R1", "false"),
        0,
        "RED on the ratified target must be recorded",
    );
}

// ---------------------------------------------------------------------------
// (6) ledger keyed by project root; cmd runs at the project root
// ---------------------------------------------------------------------------

#[test]
fn records_from_nested_subdirs_aggregate_under_the_project_root() {
    let e = Engagement::ratified(&[("R1", "first"), ("R2", "second")]);
    let ab = e.repo.subdir("a/b");
    let cd = e.repo.subdir("c/d");
    let a = e.repo.subdir("a");
    let abc = e.repo.subdir("a/b/c");

    assert_code(
        &ev(&e, &ab, "red", "R1", "test -f fixed"),
        0,
        "RED from a/b",
    );
    fs::write(e.root().join("fixed"), "").unwrap();
    assert_code(
        &ev(&e, &cd, "green", "R1", "test -f fixed"),
        0,
        "GREEN from c/d must find the RED recorded from a/b",
    );
    // Negative control: R2 still lacks evidence, so verify is incomplete
    // (a verify that always answers 0 does not satisfy this test).
    assert_code(
        &e.fde_in(&abc, &["evidence", "verify"]),
        1,
        "verify from a/b/c before R2 has evidence",
    );
    // user ruling 2026-11-04: PASS-only is not complete
    assert_code(
        &ev(&e, &a, "red", "R2", "test -f fixed2"),
        0,
        "RED R2 from a",
    );
    fs::write(e.root().join("fixed2"), "").unwrap();
    assert_code(
        &ev(&e, &a, "green", "R2", "test -f fixed2"),
        0,
        "GREEN R2 from a",
    );

    assert_code(
        &e.fde_in(&abc, &["evidence", "verify"]),
        0,
        "verify from a/b/c sees every record",
    );
    assert_code(&verify(&e), 0, "verify from the root sees every record");
    // Explicit TARGET spelled as a nested path, invoked from outside the repo.
    let ab_s = ab.to_str().unwrap();
    assert_code(
        &e.fde_in(&e.spec_dir.path, &["evidence", "verify", ab_s]),
        0,
        "verify with TARGET=a/b from outside the repo sees every record",
    );
}

#[test]
fn evidence_cmd_runs_with_cwd_at_the_project_root() {
    let e = Engagement::ratified(&[("R1", "first"), ("R2", "second")]);
    fs::write(e.root().join("root-marker"), "").unwrap();
    let nested = e.repo.subdir("deep/er");

    // Negative controls (a recorder that always answers 0 fails these): at
    // the root `test -f root-marker` passes, which contradicts RED; in the
    // nested cwd it would fail and RED would be recorded.
    assert_code(
        &ev(&e, &nested, "red", "R1", "test -f root-marker"),
        1,
        "CMD must run at the project root, so RED is contradicted",
    );
    // At the root `test ! -f root-marker` fails, which contradicts PASS.
    assert_code(
        &ev(&e, &nested, "pass", "R2", "test ! -f root-marker"),
        1,
        "CMD must run at the project root, so PASS is contradicted",
    );

    // At the root `test ! -f root-marker` FAILS (marker exists), so RED is
    // recorded. Run in the nested cwd it would pass and RED would be rejected.
    assert_code(
        &ev(&e, &nested, "red", "R1", "test ! -f root-marker"),
        0,
        "CMD must run at the project root, where root-marker exists",
    );
    // At the root `test -f root-marker` PASSES; in the nested cwd it would fail.
    assert_code(
        &ev(&e, &nested, "pass", "R2", "test -f root-marker"),
        0,
        "CMD must run at the project root, where root-marker exists",
    );
}

// ---------------------------------------------------------------------------
// (7) report
// ---------------------------------------------------------------------------

#[test]
fn report_lists_status_unresolved_and_unmeasured() {
    let e = Engagement::ratified(&[("R1", "first"), ("R2", "second"), ("R3", "third")]);
    assert_code(&ev_root(&e, "red", "R1", "test -f fixed"), 0, "RED R1");
    fs::write(e.root().join("fixed"), "").unwrap();
    assert_code(&ev_root(&e, "green", "R1", "test -f fixed"), 0, "GREEN R1");
    assert_code(&ev_root(&e, "red", "R2", "false"), 0, "RED R2");
    // R3: no evidence at all.

    let r = e.fde(&["report"]);
    assert_code(&r, 1, "report with unresolved and unmeasured requirements");
    assert!(
        !stderr(&r).trim().is_empty(),
        "report exit 1 must state a reason: {}",
        show(&r)
    );
    let text = stdout(&r);

    let r1 = lines_mentioning(&text, "R1");
    assert!(
        r1.iter().any(|l| {
            let low = l.to_lowercase();
            low.contains("red") && low.contains("green")
        }),
        "report must show R1's RED/GREEN status on its line:\n{text}"
    );
    assert!(
        !lines_mentioning(&text, "R2").is_empty(),
        "report must list R2:\n{text}"
    );

    let low = text.to_lowercase();
    let unresolved = low
        .find("unresolved")
        .unwrap_or_else(|| panic!("report must have an unresolved section:\n{text}"));
    let unmeasured = low
        .find("unmeasured")
        .unwrap_or_else(|| panic!("report must have an unmeasured section:\n{text}"));
    assert!(
        text[unresolved..].contains("R2"),
        "R2 (RED, no GREEN) must be listed as unresolved:\n{text}"
    );
    assert!(
        text[unmeasured..].contains("R3"),
        "R3 (no evidence) must be listed as unmeasured:\n{text}"
    );
    assert_no_all_clear(&text, "incomplete report");
}

#[test]
fn report_on_undetermined_ledger_never_prints_all_clear() {
    let e = Engagement::ratified(&[("R1", "first")]);
    let ledger = ledger_files_after_red(&e);
    // Control: over the intact ledger (RED only) the report is determined:
    // not done (exit 1), so the later 2 comes from the damage.
    assert_code(
        &e.fde(&["report"]),
        1,
        "report over an intact ledger with RED only",
    );
    for f in &ledger {
        fs::write(f, b"\x00\xffnot a ledger {{{[[[").unwrap();
    }
    let r = e.fde(&["report"]);
    assert_code(&r, 2, "report over a corrupt ledger is undetermined");
    assert_no_all_clear(&stdout(&r), "undetermined report");
    assert!(
        !stderr(&r).trim().is_empty(),
        "undetermined report must state a reason"
    );
}

// ---------------------------------------------------------------------------
// (8) the ledger lives outside the target
// ---------------------------------------------------------------------------

#[test]
fn ledger_is_written_outside_the_target_repo() {
    let e = Engagement::ratified(&[("R1", "first"), ("R2", "second")]);
    let before = e.repo.porcelain_ignored();
    let state_before = snapshot_files(&e.state.path);

    // The toggle file lives outside the target so the target tree stays untouched.
    let flag = e.spec_dir.path.join("fixed");
    let cmd = format!("test -f {}", flag.to_str().unwrap());
    assert_code(&ev_root(&e, "red", "R1", &cmd), 0, "RED R1");
    fs::write(&flag, "").unwrap();
    assert_code(&ev_root(&e, "green", "R1", &cmd), 0, "GREEN R1");
    assert_code(&ev_root(&e, "pass", "R2", "true"), 0, "PASS R2");
    // user ruling 2026-11-04: PASS-only is not complete
    assert_code(&verify(&e), 1, "verify: R2 is PASS-only");
    // R2 is PASS-only, so the report is determined not done (exit 1).
    assert_code(&e.fde(&["report"]), 1, "report with a PASS-only R2");

    assert_eq!(
        e.repo.porcelain_ignored(),
        before,
        "evidence must not write anything into the target repo"
    );
    let state_after = snapshot_files(&e.state.path);
    assert!(
        !changed_files(&e.state.path, &state_before, &state_after).is_empty(),
        "the ledger must be written under FDE_STATE_DIR"
    );
}
