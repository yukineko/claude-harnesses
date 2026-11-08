//! `backlog done --test` no longer EXECUTES a RED run.
//!
//! User ruling 2026-11-08, verbatim: "backlog doneのRED実行はそもそも全面廃止でいい".
//!
//! Written by an independent test writer (CLAUDE.md §2(a)); the implementer did
//! not write these. Contract under test:
//!   1. `done` creates no RED worktree and never runs CMD at REV; a CMD that
//!      PASSES at REV (and at HEAD) is ACCEPTED.
//!   2. GREEN at HEAD is unchanged: failing at HEAD refuses, store unchanged.
//!   3. `--red-rev` stays REQUIRED; an unresolvable REV is refused; a REV that
//!      is not an ancestor of HEAD is still refused.
//!   4. `closure.red` = {rev: full 40-hex, executed: false, kind: "not-executed",
//!      no `exit` key, note containing "2026-11-08"}; `closure.green` unchanged.
//!   5. `audit-closures --json` classes such a row "green-only" (not
//!      "observed-f2p"); `list` labels it "closed:green".
//!   6. `--red-overlay` is gone: passing it is a usage error.
mod common;
use common::*;

const FP: &str = "bash tests/check.sh";

/// Scenario where CMD PASSES at both REV and HEAD. Every run of the script
/// appends `"<pwd -P> <git rev-parse HEAD>"` to a log OUTSIDE the repo, so a
/// run at any other directory (e.g. a throwaway RED worktree) is observable.
/// Returns (rev, head, log_path).
fn passing_everywhere(f: &Fixture) -> (String, String, std::path::PathBuf) {
    let log = f.home.join("check-runs.log");
    let body = format!(
        "echo \"$(pwd -P) $(git rev-parse HEAD)\" >> '{}'\necho '1 passed'",
        log.display()
    );
    f.write_script("tests/check.sh", &body);
    let rev = f.commit_paths(&["tests/check.sh"], "test passes already");
    f.write_script("tests/check.sh", &format!("{body}\n# touched at HEAD"));
    let head = f.commit_paths(&["tests/check.sh"], "touch test at HEAD");
    assert_ne!(rev, head);
    assert!(!log.exists(), "fixture setup must not have run the script");
    (rev, head, log)
}

fn log_lines(log: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn cmd_passing_at_rev_is_accepted_and_runs_only_once_at_head() {
    let f = Fixture::new("abol-accept");
    let (rev, head, log) = passing_everywhere(&f);
    let id = f.add("t");
    let o = f.run(&["done", &id, "--test", FP, "--red-rev", &rev]);
    assert_eq!(
        o.code,
        0,
        "CMD passing at REV is accepted now (RED run abolished): {}",
        o.both()
    );
    assert_eq!(f.status(&id), "done");
    // Only the GREEN run happened: exactly one execution, in the fixture's own
    // working dir, at HEAD. Any REV run (worktree elsewhere / other HEAD) adds
    // a line or a different line.
    let lines = log_lines(&log);
    assert_eq!(
        lines,
        vec![format!("{} {}", f.repo.display(), head)],
        "CMD must run exactly once, at the repo working dir at HEAD; got {lines:?}"
    );
}

#[test]
fn done_creates_no_red_worktree() {
    let f = Fixture::new("abol-nowt");
    let (rev, _head, _log) = passing_everywhere(&f);
    let id = f.add("t");
    let before = f.git(&["worktree", "list", "--porcelain"]);
    let o = f.run(&["done", &id, "--test", FP, "--red-rev", &rev]);
    assert_eq!(o.code, 0, "{}", o.both());
    let after = f.git(&["worktree", "list", "--porcelain"]);
    assert_eq!(before, after, "worktree list changed across done");
    assert!(
        !after.contains("backlog-red"),
        "no backlog-red worktree may exist: {after}"
    );
    // The throwaway worktree used to live under the temp dir; the CLI is the
    // only thing in this fixture that could create one with that name.
    let stray: Vec<_> = std::fs::read_dir(std::env::temp_dir())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(&format!("backlog-red-{}-", std::process::id()))
        })
        .collect();
    assert!(stray.is_empty(), "stray RED dirs: {stray:?}");
}

#[test]
fn closure_red_records_rev_but_states_it_was_not_executed() {
    let f = Fixture::new("abol-record");
    let (rev, head, _log) = passing_everywhere(&f);
    let id = f.add("t");
    let o = f.run(&["done", &id, "--test", FP, "--red-rev", &rev]);
    assert_eq!(o.code, 0, "{}", o.both());
    let c = f.row(&id)["closure"].clone();
    let r = &c["red"];
    assert!(r.is_object(), "closure.red must exist: {c}");
    assert_eq!(rev.len(), 40);
    assert_eq!(
        r["rev"],
        rev.as_str(),
        "red.rev is the full 40-hex REV: {c}"
    );
    assert_eq!(
        r["executed"],
        serde_json::Value::Bool(false),
        "red.executed must be JSON false: {c}"
    );
    assert_eq!(r["kind"], "not-executed", "red.kind: {c}");
    assert!(
        r.as_object().unwrap().get("exit").is_none(),
        "red must carry NO `exit` key (nothing ran): {c}"
    );
    assert!(
        r["note"].as_str().is_some_and(|n| n.contains("2026-11-08")),
        "red.note must cite the 2026-11-08 ruling: {c}"
    );
    let g = &c["green"];
    assert_eq!(g["exit"], 0, "green.exit: {c}");
    assert!(g["passed"].is_number(), "green.passed: {c}");
    assert_eq!(g["rev"], head.as_str(), "green.rev == HEAD: {c}");
}

#[test]
fn short_rev_is_recorded_as_the_full_40_hex_commit() {
    let f = Fixture::new("abol-short");
    let (rev, _head, _log) = passing_everywhere(&f);
    let id = f.add("t");
    let short = &rev[..10];
    let o = f.run(&["done", &id, "--test", FP, "--red-rev", short]);
    assert_eq!(o.code, 0, "abbreviated REV resolves: {}", o.both());
    let c = f.row(&id)["closure"].clone();
    assert_eq!(c["red"]["rev"], rev.as_str(), "full 40-hex expected: {c}");
}

#[test]
fn audit_closures_classes_not_executed_red_as_green_only() {
    let f = Fixture::new("abol-audit");
    let (rev, _head, _log) = passing_everywhere(&f);
    let id = f.add("t");
    let o = f.run(&["done", &id, "--test", FP, "--red-rev", &rev]);
    assert_eq!(o.code, 0, "{}", o.both());
    let a = f.run(&["audit-closures", "--json"]);
    assert_eq!(a.code, 0, "{}", a.both());
    let v: serde_json::Value = serde_json::from_str(a.stdout.trim())
        .unwrap_or_else(|e| panic!("not json ({e}): {}", a.stdout));
    fn class_of(v: &serde_json::Value, id: &str) -> Option<String> {
        match v {
            serde_json::Value::Object(m) => {
                if m.get("id").and_then(|x| x.as_str()) == Some(id) {
                    if let Some(c) = m.get("class").and_then(|x| x.as_str()) {
                        return Some(c.to_string());
                    }
                }
                m.values().find_map(|x| class_of(x, id))
            }
            serde_json::Value::Array(a) => a.iter().find_map(|x| class_of(x, id)),
            _ => None,
        }
    }
    assert_eq!(
        class_of(&v, &id).as_deref(),
        Some("green-only"),
        "a not-executed RED is not observed F2P: {v}"
    );
}

#[test]
fn list_labels_not_executed_red_closed_green_not_closed_f2p() {
    let f = Fixture::new("abol-label");
    let (rev, _head, _log) = passing_everywhere(&f);
    let id = f.add("t");
    let o = f.run(&["done", &id, "--test", FP, "--red-rev", &rev]);
    assert_eq!(o.code, 0, "{}", o.both());
    let l = f.run(&["list", "--all"]);
    assert_eq!(l.code, 0, "{}", l.both());
    let line = l
        .stdout
        .lines()
        .find(|ln| ln.contains(&id))
        .unwrap_or_else(|| panic!("row {id} not in list output: {}", l.stdout));
    assert!(line.contains("closed:green"), "label: {line}");
    assert!(!line.contains("closed:f2p"), "must not claim f2p: {line}");
}

#[test]
fn control_failing_at_head_is_still_refused() {
    let f = Fixture::new("abol-headfail");
    let (rev, _head, _log) = passing_everywhere(&f);
    f.write_script("tests/check.sh", "echo 'FAIL at head'; exit 1");
    f.commit_paths(&["tests/check.sh"], "regress");
    let id = f.add("t");
    let before = f.store_bytes();
    let o = f.run(&["done", &id, "--test", FP, "--red-rev", &rev]);
    assert_refused_unchanged(&f, &id, "pending", &o, "GREEN at HEAD still required");
    assert_eq!(before, f.store_bytes(), "store must be byte-identical");
}

#[test]
fn red_rev_still_required() {
    let f = Fixture::new("abol-norev");
    let (_rev, _head, log) = passing_everywhere(&f);
    let id = f.add("t");
    let before = f.store_bytes();
    let o = f.run(&["done", &id, "--test", FP]);
    assert_refused_unchanged(&f, &id, "pending", &o, "missing --red-rev");
    assert_eq!(before, f.store_bytes(), "store must be byte-identical");
    let _ = log;
}

#[test]
fn control_unresolvable_rev_is_refused_nothing_written() {
    let f = Fixture::new("abol-badrev");
    let (_rev, _head, _log) = passing_everywhere(&f);
    let id = f.add("t");
    let before = f.store_bytes();
    for bad in ["nosuchrev", "0123456789abcdef0123456789abcdef01234567"] {
        let o = f.run(&["done", &id, "--test", FP, "--red-rev", bad]);
        assert_refused_unchanged(&f, &id, "pending", &o, bad);
        assert_eq!(before, f.store_bytes(), "{bad}: store changed");
    }
}

#[test]
fn non_ancestor_rev_is_still_refused() {
    let f = Fixture::new("abol-nonanc");
    let (_rev, _head, _log) = passing_everywhere(&f);
    f.git(&["checkout", "-q", "-b", "side", "HEAD~1"]);
    f.write("side.txt", "s\n");
    let side = f.commit_paths(&["side.txt"], "side commit");
    f.git(&["checkout", "-q", "main"]);
    let id = f.add("t");
    let before = f.store_bytes();
    let o = f.run(&["done", &id, "--test", FP, "--red-rev", &side]);
    assert_refused_unchanged(&f, &id, "pending", &o, "non-ancestor REV");
    assert_eq!(before, f.store_bytes(), "store must be byte-identical");
}

#[test]
fn red_overlay_flag_is_a_usage_error() {
    let f = Fixture::new("abol-overlay");
    let (rev, _head, log) = passing_everywhere(&f);
    let id = f.add("t");
    let before = f.store_bytes();
    let o = f.run(&[
        "done",
        &id,
        "--test",
        FP,
        "--red-rev",
        &rev,
        "--red-overlay",
        "tests/check.sh",
    ]);
    assert_refused_unchanged(&f, &id, "pending", &o, "--red-overlay is removed");
    assert_eq!(before, f.store_bytes(), "store must be byte-identical");
    assert!(
        log_lines(&log).is_empty(),
        "a usage error must run nothing: {:?}",
        log_lines(&log)
    );
}
