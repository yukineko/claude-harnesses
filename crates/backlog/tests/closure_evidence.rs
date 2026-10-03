//! Closure evidence: a terminal `done` needs an EXECUTED, COMMITTED test (F2P),
//! an ancestor-verified doc-only commit, a duplicate target, or a human ruling.
//! Suspicion is never evidence. Spec: close-evidence-spec.md (2026-10-01).
//!
//! Independent test writer (CLAUDE.md §2(a)); these compile against the
//! pre-feature binary (drive via Command) and FAIL at runtime until it exists.
mod common;
use common::*;

const FP: &str = "bash tests/check.sh";

fn fix_done(f: &Fixture, id: &str, red: &str) -> Out {
    f.run(&["done", id, "--test", FP, "--red-rev", red])
}

// ---- bare done / edit bypass -------------------------------------------------

#[test]
fn bare_done_is_refused_and_status_unchanged() {
    let f = Fixture::new("bare");
    let id = f.add("t");
    let o = f.run(&["done", &id]);
    assert_refused_unchanged(&f, &id, "pending", &o, "bare `done ID` has no evidence");
    assert!(!o.both().trim().is_empty(), "refusal must state a cause");
}

#[test]
fn edit_status_done_is_refused() {
    let f = Fixture::new("editdone");
    let id = f.add("t");
    let o = f.run(&["edit", &id, "--status", "done"]);
    assert_refused_unchanged(&f, &id, "pending", &o, "`edit --status done` is a bypass");
}

#[test]
fn edit_status_cancelled_is_refused() {
    let f = Fixture::new("editcancel");
    let id = f.add("t");
    let o = f.run(&["edit", &id, "--status", "cancelled"]);
    assert_refused_unchanged(&f, &id, "pending", &o, "cancelled needs a ruling");
}

// ---- runner allowlist --------------------------------------------------------

#[test]
fn allowlisted_bash_script_with_red_rev_closes_and_records_closure() {
    let f = Fixture::new("ok");
    let (red, head) = f.bug_then_fix();
    let id = f.add("t");
    let o = fix_done(&f, &id, &red);
    assert_eq!(o.code, 0, "valid F2P close must succeed: {}", o.both());
    assert_eq!(f.status(&id), "done");
    let c = f.row(&id)["closure"].clone();
    assert!(c.is_object(), "done row must carry a `closure` table: {c}");
    assert!(c["reason"].is_string(), "closure.reason missing: {c}");
    let g = &c["green"];
    assert!(g["runner"].is_string(), "green.runner: {c}");
    assert_eq!(g["cmd"], FP, "green.cmd is the exact argv text: {c}");
    assert_eq!(g["exit"], 0, "green.exit: {c}");
    assert!(g["passed"].is_number(), "green.passed: {c}");
    assert_eq!(
        g["rev"],
        head.as_str(),
        "green.rev is the full 40-hex HEAD: {c}"
    );
    assert_eq!(head.len(), 40);
    assert!(
        g["observed_at"].is_i64(),
        "green.observed_at must be an integer: {c}"
    );
    assert!(
        g["output_digest"].as_str().is_some_and(|d| !d.is_empty()),
        "green.output_digest: {c}"
    );
    assert!(g["excerpt"].as_str().is_some(), "green.excerpt: {c}");
    let r = &c["red"];
    assert_eq!(r["rev"], red.as_str(), "red.rev: {c}");
    assert_ne!(r["exit"], 0, "red.exit must be non-zero: {c}");
    assert_eq!(r["kind"], "behavioural", "red.kind: {c}");
}

#[test]
fn excerpt_is_capped_at_4096_bytes() {
    let f = Fixture::new("excerpt");
    let (red, _) = f.bug_then_fix();
    f.write_script(
        "tests/check.sh",
        "grep -q '^fixed$' src/impl.txt || exit 1\nhead -c 20000 /dev/zero | tr '\\0' 'x'; echo; echo '1 passed'",
    );
    f.commit_paths(&["tests/check.sh"], "noisy test");
    let id = f.add("t");
    let o = fix_done(&f, &id, &red);
    assert_eq!(o.code, 0, "{}", o.both());
    let ex = f.row(&id)["closure"]["green"]["excerpt"]
        .as_str()
        .unwrap()
        .len();
    assert!(ex > 0 && ex <= 4096, "excerpt len {ex} must be in 1..=4096");
}

#[test]
fn shell_metacharacters_are_refused() {
    let f = Fixture::new("meta");
    let (red, _) = f.bug_then_fix();
    let id = f.add("t");
    for cmd in [
        "bash tests/check.sh; true",
        "bash tests/check.sh && true",
        "bash tests/check.sh | cat",
        "bash tests/check.sh > /dev/null",
        "bash tests/check.sh $(true)",
        "bash tests/check.sh `true`",
        "bash tests/check.sh || true",
    ] {
        let o = f.run(&["done", &id, "--test", cmd, "--red-rev", &red]);
        assert_refused_unchanged(&f, &id, "pending", &o, cmd);
    }
}

#[test]
fn grep_and_cat_are_not_tests() {
    let f = Fixture::new("grep");
    let (red, _) = f.bug_then_fix();
    let id = f.add("t");
    for cmd in [
        "grep -q fixed src/impl.txt",
        "cat src/impl.txt",
        "true",
        "echo ok",
    ] {
        let o = f.run(&["done", &id, "--test", cmd, "--red-rev", &red]);
        assert_refused_unchanged(&f, &id, "pending", &o, cmd);
    }
}

#[test]
fn bash_script_must_be_git_tracked_and_under_a_tests_dir() {
    let f = Fixture::new("tracked");
    let (red, _) = f.bug_then_fix();
    let id = f.add("t");
    // present on disk but never committed
    f.write_script("tests/untracked.sh", "exit 0");
    let o = f.run(&[
        "done",
        &id,
        "--test",
        "bash tests/untracked.sh",
        "--red-rev",
        &red,
    ]);
    assert_refused_unchanged(&f, &id, "pending", &o, "untracked script");
    // committed but NOT under a tests dir
    f.write_script("scripts/notatest.sh", "exit 0");
    f.commit_paths(&["scripts/notatest.sh"], "script outside tests");
    let o = f.run(&[
        "done",
        &id,
        "--test",
        "bash scripts/notatest.sh",
        "--red-rev",
        &red,
    ]);
    assert_refused_unchanged(&f, &id, "pending", &o, "script outside tests dir");
}

#[test]
fn fake_cargo_test_is_on_the_allowlist() {
    let f = Fixture::new("cargook");
    let (red, _) = f.bug_then_fix();
    let id = f.add("t");
    let o = f.run(&["done", &id, "--test", "cargo test -p x", "--red-rev", &red]);
    assert_eq!(
        o.code,
        0,
        "cargo test is allowlisted, RED=broken marker (behavioural): {}",
        o.both()
    );
    assert_eq!(f.status(&id), "done");
    assert!(f.row(&id)["closure"]["green"]["passed"].as_i64().unwrap() >= 1);
}

#[test]
fn zero_passed_is_refused() {
    let f = Fixture::new("zero");
    let (red, _) = f.bug_then_fix();
    f.write("marker", "empty");
    f.commit_paths(&["marker"], "marker empty");
    let id = f.add("t");
    let o = f.run(&["done", &id, "--test", "cargo test -p x", "--red-rev", &red]);
    assert_refused_unchanged(
        &f,
        &id,
        "pending",
        &o,
        "exit 0 with 0 passed proves nothing",
    );
}

#[test]
fn zero_passed_output_from_a_bash_test_is_refused() {
    let f = Fixture::new("zerobash");
    let (red, _) = f.bug_then_fix();
    f.write_script(
        "tests/check.sh",
        "grep -q '^fixed$' src/impl.txt || exit 1\necho 'test result: ok. 0 passed; 0 failed'",
    );
    f.commit_paths(&["tests/check.sh"], "zero-passed test");
    let id = f.add("t");
    let o = fix_done(&f, &id, &red);
    assert_refused_unchanged(&f, &id, "pending", &o, "0 passed in output");
}

#[test]
fn failing_test_at_head_is_refused() {
    let f = Fixture::new("headfail");
    let (red, _) = f.bug_then_fix();
    f.write("src/impl.txt", "broken again\n");
    f.commit_paths(&["src/impl.txt"], "regress");
    let id = f.add("t");
    let o = fix_done(&f, &id, &red);
    assert_refused_unchanged(&f, &id, "pending", &o, "non-zero at HEAD");
}

#[test]
fn timeout_is_refused_with_status_unchanged() {
    let f = Fixture::new("timeout");
    let (red, _) = f.bug_then_fix();
    f.write_script("tests/check.sh", "sleep 30\nexit 0");
    f.commit_paths(&["tests/check.sh"], "slow test");
    let id = f.add("t");
    let t = std::time::Instant::now();
    let o = f.run_env(
        &["done", &id, "--test", FP, "--red-rev", &red],
        &[("BACKLOG_TEST_TIMEOUT_SECS", "1")],
    );
    assert!(
        t.elapsed().as_secs() < 25,
        "timeout must actually kill the runner"
    );
    assert_refused_unchanged(&f, &id, "pending", &o, "timeout is Undetermined");
}

#[test]
fn spawn_failure_is_refused() {
    let f = Fixture::new("spawn");
    let (red, _) = f.bug_then_fix();
    f.write_exec_shim("cargo", "#!/nonexistent/interpreter\n");
    let id = f.add("t");
    let o = f.run(&["done", &id, "--test", "cargo test -p x", "--red-rev", &red]);
    assert_refused_unchanged(&f, &id, "pending", &o, "spawn failure is Undetermined");
}

#[test]
fn dirty_tree_outside_backlog_is_refused() {
    let f = Fixture::new("dirty");
    let (red, _) = f.bug_then_fix();
    let id = f.add("t");
    f.write("README.txt", "locally modified, uncommitted\n");
    let o = fix_done(&f, &id, &red);
    assert_refused_unchanged(
        &f,
        &id,
        "pending",
        &o,
        "tests must run against committed content",
    );
}

// ---- F2P ---------------------------------------------------------------------

#[test]
fn worked_fix_without_red_rev_is_refused() {
    let f = Fixture::new("nored");
    f.bug_then_fix();
    let id = f.add("t");
    let o = f.run(&["done", &id, "--test", FP]);
    assert_refused_unchanged(&f, &id, "pending", &o, "worked fix needs --red-rev");
}

#[test]
fn already_fixed_without_red_rev_is_refused() {
    let f = Fixture::new("afnored");
    f.bug_then_fix();
    let id = f.add("t");
    let o = f.run(&["done", &id, "--test", FP, "--reason", "already-fixed"]);
    assert_refused_unchanged(&f, &id, "pending", &o, "already-fixed still needs F2P");
}

#[test]
fn already_fixed_with_red_rev_is_accepted_and_recorded() {
    let f = Fixture::new("afok");
    let (red, _) = f.bug_then_fix();
    let id = f.add("t");
    let o = f.run(&[
        "done",
        &id,
        "--test",
        FP,
        "--red-rev",
        &red,
        "--reason",
        "already-fixed",
    ]);
    assert_eq!(o.code, 0, "{}", o.both());
    assert_eq!(f.row(&id)["closure"]["reason"], "already-fixed");
}

#[test]
fn red_rev_where_test_already_passes_is_refused() {
    let f = Fixture::new("notred");
    let (_, head) = f.bug_then_fix();
    let id = f.add("t");
    let o = fix_done(&f, &id, &head);
    assert_refused_unchanged(&f, &id, "pending", &o, "test passing at red-rev is not RED");
}

#[test]
fn build_failure_red_is_not_accepted() {
    let f = Fixture::new("buildred");
    f.write("marker", "nobuild");
    let a = f.commit_paths(&["marker"], "pre-feature: api absent");
    f.write("marker", "fixed");
    f.commit_paths(&["marker"], "feature lands");
    let id = f.add("feature");
    let o = f.run(&["done", &id, "--test", "cargo test -p x", "--red-rev", &a]);
    assert_refused_unchanged(
        &f,
        &id,
        "pending",
        &o,
        "RED that fails at BUILD is not behavioural",
    );
    let m = o.both().to_lowercase();
    assert!(
        m.contains("build") || m.contains("compile") || m.contains("ruling"),
        "refusal must name the build-failure cause / ruling route: {m}"
    );
}

#[test]
fn behavioural_red_under_fake_cargo_is_accepted() {
    let f = Fixture::new("behavred");
    f.write("marker", "broken");
    let a = f.commit_paths(&["marker"], "buggy");
    f.write("marker", "fixed");
    f.commit_paths(&["marker"], "fixed");
    let id = f.add("t");
    let o = f.run(&["done", &id, "--test", "cargo test -p x", "--red-rev", &a]);
    assert_eq!(o.code, 0, "{}", o.both());
    assert_eq!(f.row(&id)["closure"]["red"]["kind"], "behavioural");
}

#[test]
fn red_rev_must_be_an_ancestor_of_head() {
    let f = Fixture::new("ancestry");
    let (_, _) = f.bug_then_fix();
    f.git(&["checkout", "-q", "-b", "side", "HEAD~1"]);
    f.write("side.txt", "s\n");
    let side = f.commit_paths(&["side.txt"], "side commit");
    f.git(&["checkout", "-q", "main"]);
    let id = f.add("t");
    let o = fix_done(&f, &id, &side);
    assert_refused_unchanged(&f, &id, "pending", &o, "non-ancestor red-rev");
    let o = fix_done(&f, &id, "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef");
    assert_refused_unchanged(&f, &id, "pending", &o, "nonexistent red-rev");
    let o = fix_done(&f, &id, "not-a-rev");
    assert_refused_unchanged(&f, &id, "pending", &o, "garbage red-rev");
}

#[test]
fn red_run_leaves_no_temp_worktree_behind() {
    let f = Fixture::new("wtclean");
    let (red, _) = f.bug_then_fix();
    let id = f.add("t");
    let o = fix_done(&f, &id, &red);
    assert_eq!(o.code, 0, "{}", o.both());
    let wts = f.git(&["worktree", "list", "--porcelain"]);
    assert_eq!(
        wts.matches("worktree ").count(),
        1,
        "temp worktree leaked: {wts}"
    );
    assert_eq!(
        f.git(&["status", "--porcelain", "--", ".", ":!.backlog"]),
        "",
        "tree dirtied"
    );
}

// ---- obsolete ----------------------------------------------------------------

fn obsolete_scenario(f: &Fixture) -> String {
    f.write("old.txt", "legacy surface\n");
    let a = f.commit_paths(&["old.txt"], "old surface exists");
    f.commit_rm("old.txt", "old surface removed");
    f.write_script(
        "tests/gone.sh",
        "test ! -e old.txt || { echo 'old surface still present'; exit 1; }\necho '1 passed'",
    );
    f.commit_paths(&["tests/gone.sh"], "test old surface is gone");
    a
}

#[test]
fn obsolete_with_committed_test_and_red_rev_is_accepted() {
    let f = Fixture::new("obs");
    let a = obsolete_scenario(&f);
    let id = f.add("t");
    let o = f.run(&[
        "done",
        &id,
        "--reason",
        "obsolete",
        "--test",
        "bash tests/gone.sh",
        "--red-rev",
        &a,
    ]);
    assert_eq!(o.code, 0, "{}", o.both());
    assert_eq!(f.row(&id)["closure"]["reason"], "obsolete");
}

#[test]
fn obsolete_without_test_or_without_red_rev_is_refused() {
    let f = Fixture::new("obsno");
    let a = obsolete_scenario(&f);
    let id = f.add("t");
    let o = f.run(&["done", &id, "--reason", "obsolete"]);
    assert_refused_unchanged(&f, &id, "pending", &o, "obsolete without test");
    let o = f.run(&[
        "done",
        &id,
        "--reason",
        "obsolete",
        "--test",
        "bash tests/gone.sh",
    ]);
    assert_refused_unchanged(&f, &id, "pending", &o, "obsolete without red-rev");
    let _ = a;
}

// ---- duplicate ---------------------------------------------------------------

#[test]
fn duplicate_of_pending_target_is_accepted() {
    let f = Fixture::new("dup");
    let a = f.add("canonical");
    let b = f.add("the other one");
    let o = f.run(&["done", &b, "--duplicate-of", &a]);
    assert_eq!(o.code, 0, "{}", o.both());
    assert_eq!(f.status(&b), "done");
    assert_eq!(f.row(&b)["closure"]["duplicate_of"], a.as_str());
}

/// CONTRACT ALIGNMENT (close-evidence port): the third case used to assert that
/// a legacy `done` row with no closure table is REFUSED as a duplicate target.
/// That contradicts the spec this file cites (close-evidence-spec.md, DUPLICATE:
/// "a legacy done row without a closure table counts; user ruling 2026-10-01,
/// matches gate case P8"), `closecmd::check_duplicate_target`'s doc, and
/// `scripts/tests/closure-evidence-gate.sh` P8 — and it was RED at the branch
/// tip 04ea9b35 itself. It now pins the ruled behaviour: accepted, with the
/// target recorded. The missing-target and self-duplicate refusals are kept.
#[test]
fn duplicate_of_missing_or_self_is_refused_legacy_done_target_is_accepted() {
    let f = Fixture::new("dupbad");
    let b = f.add("x");
    let o = f.run(&["done", &b, "--duplicate-of", "00000000"]);
    assert_refused_unchanged(&f, &b, "pending", &o, "missing target");
    let o = f.run(&["done", &b, "--duplicate-of", &b]);
    assert_refused_unchanged(&f, &b, "pending", &o, "self-duplicate");
    f.plant_legacy_done("a1b2c3d4", "legacy", "done");
    let o = f.run(&["done", &b, "--duplicate-of", "a1b2c3d4"]);
    assert_eq!(
        o.code,
        0,
        "a legacy done row counts as a duplicate target (user ruling 2026-10-01): {}",
        o.both()
    );
    assert_eq!(f.status(&b), "done");
    assert_eq!(
        f.row(&b)["closure"]["duplicate_of"],
        "a1b2c3d4",
        "the duplicate target must be recorded"
    );
}

#[test]
fn duplicate_of_unreadable_store_is_refused() {
    let f = Fixture::new("dupunread");
    let a = f.add("canonical");
    let b = f.add("other");
    std::fs::write(f.done_path(), "this is [[[ not toml").unwrap();
    let o = f.run(&["done", &b, "--duplicate-of", &a]);
    assert_ne!(o.code, 0, "unreadable store must refuse: {}", o.both());
    std::fs::remove_file(f.done_path()).unwrap();
    assert_eq!(f.status(&b), "pending");
}

#[test]
fn duplicate_of_done_with_evidence_target_is_accepted() {
    let f = Fixture::new("dupev");
    let (red, _) = f.bug_then_fix();
    let a = f.add("canonical");
    assert_eq!(fix_done(&f, &a, &red).code, 0);
    let b = f.add("other");
    let o = f.run(&["done", &b, "--duplicate-of", &a]);
    assert_eq!(
        o.code,
        0,
        "done-with-evidence target is valid: {}",
        o.both()
    );
}
