// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Contract C1b: re-ratification starts a new evidence epoch, plus the
//! previously untested `Undetermined` arms of the ratification / ledger
//! readers.
//!
//! Written by an independent test author (CLAUDE.md §2(a)); the implementer
//! must match the contract, not edit these tests to pass.
//!
//! Contract under test (coordinator-decided):
//!
//! 1. Every new evidence record carries `ratified_head`: the ratification head
//!    R in force when it was recorded (40 lowercase hex).
//! 2. A record counts only when its `fingerprint` equals the current ratified
//!    fingerprint AND its `ratified_head` equals the current ratification
//!    head. Others are stale: ignored by verify / report / recording
//!    preconditions, and counted in report's "ignored: N record(s) from an
//!    earlier ratification" line.
//! 3. ratify at A, RED+GREEN, `git reset --hard` to a non-descendant B,
//!    verify -> 2; `fde ratify` again (head B) -> verify 1, report 1 with the
//!    ignored records mentioned; a new RED+GREEN at B -> verify/report 0.
//! 4. Re-ratifying at the same HEAD keeps the records counting: verify 0.
//! 5. A fingerprint-matching record without `ratified_head` (legacy) ->
//!    verify / report 2 with a reason. No epoch is guessed for it.
//! 6. Previously untested `Undetermined` arms:
//!    - (a) schema-1 ledger under a schema-2 ratification -> verify 2,
//!      report 2.
//!    - (b) schema-2 ledger record with `head` but not `dirty` (or vice
//!      versa) -> verify 2.
//!    - (c) schema-2 ratification with a malformed `head` -> status 2,
//!      verify 2.
//!    - (d) schema-2 ratification without `dirty` -> status 2.
//!    - (e) schema-1 ratification carrying `head` -> status 2.
//!
//! `--cmd` values are `test -f <flag>` with the flag outside the target, so
//! the target tree is never dirtied by the tests themselves.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use common::{code, git, parse_resolved, show, stderr, stdout, Engagement};

// ------------------------------------------------------------------ helpers

fn assert_code(out: &Output, want: i32, what: &str) {
    assert_eq!(code(out), Some(want), "{what}: {}", show(out));
}

/// Exit 2 with a non-empty stderr reason.
fn assert_undetermined(out: &Output, what: &str) {
    assert_code(out, 2, what);
    assert!(
        !stderr(out).trim().is_empty(),
        "{what}: exit 2 must state a reason on stderr: {}",
        show(out)
    );
}

/// Exit 1 with a non-empty stderr reason.
fn assert_not_done(out: &Output, what: &str) {
    assert_code(out, 1, what);
    assert!(
        !stderr(out).trim().is_empty(),
        "{what}: exit 1 must state a reason on stderr: {}",
        show(out)
    );
}

fn ev(e: &Engagement, kind: &str, req: &str, cmd: &str) -> Output {
    e.fde(&["evidence", kind, "--req", req, "--cmd", cmd])
}

fn verify(e: &Engagement) -> Output {
    e.fde(&["evidence", "verify"])
}

fn report(e: &Engagement) -> Output {
    e.fde(&["report"])
}

fn status(e: &Engagement) -> Output {
    e.fde(&["status"])
}

/// RED (flag absent) then GREEN (flag created) for `req`, using a flag file
/// named `flag` outside the target. Each call must use a fresh flag name.
fn red_then_green(e: &Engagement, req: &str, flag: &str) {
    let flag = e.spec_dir.path.join(flag);
    assert!(
        !flag.exists(),
        "fixture: flag {} must be fresh",
        flag.display()
    );
    let cmd = format!("test -f {}", flag.to_str().unwrap());
    assert_code(&ev(e, "red", req, &cmd), 0, &format!("RED {req}"));
    fs::write(&flag, "").unwrap();
    assert_code(&ev(e, "green", req, &cmd), 0, &format!("GREEN {req}"));
}

/// `<state>/targets/<state_key>` of the engagement's target.
fn target_state_dir(e: &Engagement) -> PathBuf {
    let (_root, key) = parse_resolved(&e.fde(&["resolve"]));
    e.state.path.join("targets").join(key)
}

fn ratification_path(e: &Engagement) -> PathBuf {
    target_state_dir(e).join("ratification.json")
}

fn ledger_path(e: &Engagement) -> PathBuf {
    target_state_dir(e).join("evidence.json")
}

fn read_json(p: &Path) -> serde_json::Value {
    let bytes = fs::read(p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()));
    serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("parse {}: {e}", p.display()))
}

fn write_json(p: &Path, v: &serde_json::Value) {
    fs::write(p, serde_json::to_vec_pretty(v).unwrap()).unwrap();
}

/// Read the JSON at `p`, let `f` modify it, write it back.
fn edit_json(p: &Path, f: impl FnOnce(&mut serde_json::Value)) {
    let mut v = read_json(p);
    f(&mut v);
    write_json(p, &v);
}

/// The ledger's `records` array, mutable.
fn records_mut(v: &mut serde_json::Value) -> &mut Vec<serde_json::Value> {
    v.get_mut("records")
        .and_then(|r| r.as_array_mut())
        .expect("evidence.json has an array \"records\"")
}

fn obj_mut(v: &mut serde_json::Value) -> &mut serde_json::Map<String, serde_json::Value> {
    v.as_object_mut().expect("a JSON object")
}

/// `git rev-parse HEAD` at the target root (40 hex).
fn head_of(e: &Engagement) -> String {
    git(e.root(), &["rev-parse", "HEAD"]).trim().to_owned()
}

/// A parentless commit (same tree as HEAD) kept reachable through
/// `refs/heads/unrelated`: a non-descendant of every existing commit.
fn unrelated_commit(e: &Engagement) -> String {
    let tree = git(e.root(), &["rev-parse", "HEAD^{tree}"])
        .trim()
        .to_owned();
    let id = git(e.root(), &["commit-tree", &tree, "-m", "unrelated root"])
        .trim()
        .to_owned();
    git(e.root(), &["update-ref", "refs/heads/unrelated", &id]);
    id
}

/// Commit a new file `name` in the target; returns the new HEAD.
fn commit_file(e: &Engagement, name: &str) -> String {
    fs::write(e.root().join(name), format!("{name}\n")).unwrap();
    git(e.root(), &["add", name]);
    git(e.root(), &["commit", "-q", "-m", name]);
    head_of(e)
}

fn is_hex40(s: &str) -> bool {
    s.len() == 40
        && s.chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

/// A ratified one-requirement target with a complete RED+GREEN for R1
/// (verify 0, report 0 as the positive control).
fn complete_r1() -> Engagement {
    let e = Engagement::ratified(&[("R1", "first")]);
    red_then_green(&e, "R1", "flag-a");
    assert_code(&verify(&e), 0, "control: verify after RED+GREEN");
    assert_code(&report(&e), 0, "control: report after RED+GREEN");
    e
}

// ------------------------------------------------- (1) ratified_head recorded

#[test]
fn every_new_record_carries_the_ratification_head_as_ratified_head() {
    let e = complete_r1();
    let r = read_json(&ratification_path(&e))
        .get("head")
        .and_then(|h| h.as_str())
        .expect("ratification.json has a string head")
        .to_owned();
    let v = read_json(&ledger_path(&e));
    let recs = v.get("records").and_then(|r| r.as_array()).unwrap();
    assert_eq!(recs.len(), 2, "RED + GREEN: {v}");
    for rec in recs {
        let rh = rec
            .get("ratified_head")
            .and_then(|h| h.as_str())
            .unwrap_or_else(|| panic!("evidence record lacks string \"ratified_head\": {rec}"));
        assert!(
            is_hex40(rh),
            "ratified_head must be 40 lowercase hex: {rec}"
        );
        assert_eq!(rh, r, "ratified_head must be the ratification head: {rec}");
    }
}

// ------------------------------------- (2)/(3) re-ratification = a new epoch

#[test]
fn reset_off_the_ratified_line_then_reratify_starts_a_new_epoch() {
    let e = complete_r1();
    let a = head_of(&e);
    let b = unrelated_commit(&e);
    git(e.root(), &["reset", "-q", "--hard", &b]);
    assert_ne!(a, b, "fixture: B differs from A");

    // Off the ratified line: undetermined (unchanged behaviour).
    assert_undetermined(&verify(&e), "verify after reset to a non-descendant");

    // A human re-ratifies the same spec at B.
    e.ratify();
    let r = read_json(&ratification_path(&e));
    assert_eq!(
        r.get("head").and_then(|h| h.as_str()),
        Some(b.as_str()),
        "fixture: ratification head is now B: {r}"
    );

    // The A-epoch records are stale: requirements unmeasured -> 1, not 2.
    let v = verify(&e);
    assert_not_done(&v, "verify after re-ratify at B (old records stale)");
    assert!(
        stderr(&v).contains("R1"),
        "verify must name the unmeasured R1: {}",
        show(&v)
    );
    let rep = report(&e);
    assert_not_done(&rep, "report after re-ratify at B (old records stale)");
    assert!(
        stdout(&rep).contains("ignored: 2 record(s) from an earlier ratification"),
        "report must count the 2 stale A-epoch records on its ignored line: {}",
        show(&rep)
    );

    // New evidence at B completes the new epoch.
    red_then_green(&e, "R1", "flag-b");
    assert_code(&verify(&e), 0, "verify after RED+GREEN at B");
    let rep = report(&e);
    assert_code(&rep, 0, "report after RED+GREEN at B");
    assert!(
        stdout(&rep).contains("ignored: 2 record(s) from an earlier ratification"),
        "the stale A-epoch records stay ignored (and counted): {}",
        show(&rep)
    );
}

#[test]
fn stale_epoch_red_does_not_block_a_pass_after_reratify() {
    // Recording preconditions ignore stale records too: a PASS needs "no RED
    // for the requirement", and the only RED is from the earlier epoch.
    let e = complete_r1();
    let b = unrelated_commit(&e);
    git(e.root(), &["reset", "-q", "--hard", &b]);
    e.ratify();
    let out = ev(&e, "pass", "R1", "true");
    assert_code(&out, 0, "PASS for R1 after re-ratify (old RED is stale)");
    // user ruling 2026-11-04: PASS-only is not complete. The new-epoch PASS
    // still counts as evidence (verify names it PASS-only, not "no evidence").
    let v = verify(&e);
    assert_code(&v, 1, "verify: R1 is PASS-only in the new epoch");
    assert!(
        stderr(&v).contains("PASS"),
        "verify must say R1 is PASS-only: {}",
        show(&v)
    );
}

#[test]
fn stale_epoch_red_does_not_satisfy_a_green_after_reratify() {
    // A GREEN needs a RED of the same CMD in the CURRENT epoch; the one from
    // the earlier epoch must not count.
    let e = Engagement::ratified(&[("R1", "first")]);
    let flag = e.spec_dir.path.join("flag-stale");
    let cmd = format!("test -f {}", flag.to_str().unwrap());
    assert_code(&ev(&e, "red", "R1", &cmd), 0, "RED in epoch A");
    let b = unrelated_commit(&e);
    git(e.root(), &["reset", "-q", "--hard", &b]);
    e.ratify();
    fs::write(&flag, "").unwrap();
    let out = ev(&e, "green", "R1", &cmd);
    assert_code(&out, 1, "GREEN in epoch B without a RED in epoch B");
    let v = read_json(&ledger_path(&e));
    assert_eq!(
        v.get("records").and_then(|r| r.as_array()).map(Vec::len),
        Some(1),
        "the refused GREEN records nothing: {v}"
    );
}

#[test]
fn reratify_at_a_forward_commit_also_starts_a_new_epoch() {
    // Item 2: counting needs ratified_head == current R, even when the new R
    // is a descendant of the old one.
    let e = complete_r1();
    commit_file(&e, "forward.txt");
    assert_code(&verify(&e), 0, "control: forward commit keeps verify 0");
    e.ratify();
    let v = verify(&e);
    assert_not_done(&v, "verify after re-ratify at a forward commit");
    let rep = report(&e);
    assert_not_done(&rep, "report after re-ratify at a forward commit");
    assert!(
        stdout(&rep).contains("ignored: 2 record(s) from an earlier ratification"),
        "{}",
        show(&rep)
    );
}

// ----------------------------------------------- (4) same-HEAD re-ratify

#[test]
fn reratify_at_the_same_head_keeps_records_counting() {
    let e = complete_r1();
    let before = read_json(&ratification_path(&e));
    e.ratify();
    let after = read_json(&ratification_path(&e));
    assert_eq!(
        before.get("head"),
        after.get("head"),
        "fixture: same HEAD re-ratified"
    );
    assert_code(&verify(&e), 0, "verify after same-HEAD re-ratify");
    let rep = report(&e);
    assert_code(&rep, 0, "report after same-HEAD re-ratify");
    assert!(
        !stdout(&rep).contains("ignored:"),
        "no record is stale after a same-HEAD re-ratify: {}",
        show(&rep)
    );
}

// --------------------------------------- (5) legacy record, no ratified_head

#[test]
fn record_without_ratified_head_is_undetermined_for_verify_and_report() {
    let e = complete_r1();
    edit_json(&ledger_path(&e), |v| {
        for rec in records_mut(v) {
            let o = obj_mut(rec);
            o.remove("ratified_head");
            assert!(
                o.contains_key("head") && o.contains_key("dirty"),
                "fixture: head/dirty kept, only ratified_head removed: {rec:?}",
                rec = o
            );
        }
    });
    assert_undetermined(&verify(&e), "verify with legacy (no ratified_head) records");
    let rep = report(&e);
    assert_undetermined(&rep, "report with legacy (no ratified_head) records");
}

// ------------------------------------------------ (6a) schema-1 ledger

#[test]
fn schema1_ledger_under_schema2_ratification_is_undetermined() {
    let e = complete_r1();
    edit_json(&ledger_path(&e), |v| {
        obj_mut(v).insert("schema".into(), serde_json::json!(1));
        for rec in records_mut(v) {
            let o = obj_mut(rec);
            o.remove("head");
            o.remove("dirty");
            o.remove("ratified_head");
        }
    });
    // Control: the ratification is schema 2 and still ratified.
    let r = read_json(&ratification_path(&e));
    assert_eq!(r.get("schema"), Some(&serde_json::json!(2)), "{r}");
    assert_code(&status(&e), 0, "control: still ratified");
    assert_undetermined(&verify(&e), "verify over a schema-1 ledger");
    assert_undetermined(&report(&e), "report over a schema-1 ledger");
}

// ------------------------------------- (6b) half-provenance schema-2 record

#[test]
fn schema2_record_with_head_but_no_dirty_is_undetermined() {
    let e = complete_r1();
    edit_json(&ledger_path(&e), |v| {
        for rec in records_mut(v) {
            obj_mut(rec).remove("dirty");
        }
    });
    assert_undetermined(&verify(&e), "verify: record has head but no dirty");
}

#[test]
fn schema2_record_with_dirty_but_no_head_is_undetermined() {
    let e = complete_r1();
    edit_json(&ledger_path(&e), |v| {
        for rec in records_mut(v) {
            obj_mut(rec).remove("head");
        }
    });
    assert_undetermined(&verify(&e), "verify: record has dirty but no head");
}

// ----------------------------------- (6c) malformed ratification head

fn with_ratification_head(e: &Engagement, head: &str) {
    edit_json(&ratification_path(e), |v| {
        obj_mut(v).insert("head".into(), serde_json::json!(head));
    });
}

#[test]
fn ratification_head_too_short_is_undetermined() {
    let e = complete_r1();
    with_ratification_head(&e, "abc");
    assert_undetermined(&status(&e), "status with ratification head \"abc\"");
    assert_undetermined(&verify(&e), "verify with ratification head \"abc\"");
}

#[test]
fn ratification_head_uppercase_is_undetermined() {
    let e = complete_r1();
    let upper = head_of(&e).to_uppercase();
    assert!(
        upper.chars().any(|c| c.is_ascii_uppercase()),
        "fixture: the HEAD id must contain a letter to uppercase: {upper}"
    );
    with_ratification_head(&e, &upper);
    assert_undetermined(&status(&e), "status with an uppercase ratification head");
    assert_undetermined(&verify(&e), "verify with an uppercase ratification head");
}

// ----------------------------------- (6d) schema-2 ratification w/o dirty

#[test]
fn schema2_ratification_without_dirty_is_undetermined() {
    let e = complete_r1();
    edit_json(&ratification_path(&e), |v| {
        assert!(obj_mut(v).remove("dirty").is_some(), "fixture: had dirty");
    });
    assert_undetermined(&status(&e), "status: schema-2 ratification without dirty");
}

// ----------------------------------- (6e) schema-1 ratification with head

#[test]
fn schema1_ratification_carrying_head_is_undetermined() {
    let e = complete_r1();
    edit_json(&ratification_path(&e), |v| {
        let o = obj_mut(v);
        o.insert("schema".into(), serde_json::json!(1));
        o.remove("dirty");
        assert!(o.contains_key("head"), "fixture: keeps head");
    });
    // Control: the same file without head is a legal legacy ratification.
    let p = ratification_path(&e);
    let with_head = read_json(&p);
    edit_json(&p, |v| {
        obj_mut(v).remove("head");
    });
    assert_code(&status(&e), 0, "control: schema-1 without head is ratified");
    write_json(&p, &with_head);
    assert_undetermined(&status(&e), "status: schema-1 ratification carrying head");
}
