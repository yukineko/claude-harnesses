//! End-to-end coverage for `condukt review-brief`: given a run id + task id,
//! it must print a DETERMINISTIC reviewer digest composed from static,
//! already-persisted signals — the decomposition intent, the task's declared
//! touched_files/target_symbols, and any overwatch violation matched by this
//! task's `task_key`.
//!
//! Isolation mirrors `diffrisk_record_e2e.rs`: the binary is spawned against
//! an isolated `HOME` so condukt's state (`<home>/.condukt`) and overwatch's
//! violation ledger (`<home>/.overwatch`) land in a throwaway dir — never
//! the real store.
//!
//! The tripped-invariant fixture is seeded via the SAME real post-execution
//! diff-risk pipeline `diffrisk_record_e2e.rs` proves (a `state set --status
//! done` transition on a High-risk worktree), so the violation ledger entry
//! is genuine, not hand-crafted JSON.

use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

struct Fixture {
    base: PathBuf,
    repo: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let pid = std::process::id();
        let mut base = std::env::temp_dir();
        base.push(format!("condukt-review-brief-{pid}-{tag}"));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        run_git(&repo, &["init", "-q", "-b", "main"]);
        run_git(&repo, &["config", "user.email", "t@t.t"]);
        run_git(&repo, &["config", "user.name", "t"]);
        Self { base, repo, home }
    }

    fn condukt(&self, args: &[&str]) -> std::process::Output {
        Command::new(bin())
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .output()
            .expect("spawn condukt")
    }

    fn init_run(&self, decomp_json: &str) -> String {
        let p = self.repo.join("decomp.json");
        std::fs::write(&p, decomp_json).unwrap();
        let out = self.condukt(&["state", "init", "--file", p.to_str().unwrap()]);
        assert!(out.status.success(), "init failed: {out:?}");
        run_id_from(&out)
    }
}

fn run_git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git")
        .status
        .success();
    assert!(ok, "git {args:?} failed");
}

fn run_id_from(out: &std::process::Output) -> String {
    let s = String::from_utf8_lossy(&out.stdout);
    s.lines()
        .chain(String::from_utf8_lossy(&out.stderr).lines())
        .rev()
        .map(str::trim)
        .find(|l| l.starts_with("run-"))
        .expect("a run- id in init output")
        .to_string()
}

/// Seed `main` with the initial (pre-change) version of the sensitive file,
/// once per fixture. Idempotent-safe to call before each
/// [`build_highrisk_worktree`] batch, but callers that need MULTIPLE
/// distractor worktrees off the same unchanged `main` must call this only
/// ONCE (a second no-op write + commit on an unchanged file fails — nothing
/// to commit).
fn seed_sensitive_file(fx: &Fixture, sensitive_rel: &str) {
    let file = fx.repo.join(sensitive_rel);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "pub fn old_api(x: i32) {}\n").unwrap();
    run_git(&fx.repo, &["add", "-A"]);
    run_git(&fx.repo, &["commit", "-q", "-m", "seed"]);
}

/// Build a worktree branch whose diff-vs-main changes a `pub fn` signature on
/// a SENSITIVE path (a `hooks/` file), so blastguard's `classify_diff` grades
/// it High (public-API signal AND sensitive-path signal both fire) and the
/// post-execution hook records a real `blastguard:diffrisk-public-api`
/// violation keyed `"<run_id>/<task_id>"`. Assumes [`seed_sensitive_file`]
/// already ran for `sensitive_rel` on `main`.
fn build_highrisk_worktree(fx: &Fixture, sensitive_rel: &str, wt_name: &str) -> PathBuf {
    let wt = fx.base.join(wt_name);
    let branch = format!("condukt/{wt_name}");
    run_git(
        &fx.repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            &branch,
            wt.to_str().unwrap(),
            "main",
        ],
    );
    let wt_file = wt.join(sensitive_rel);
    std::fs::write(&wt_file, "pub fn new_api(x: i32, y: i32) {}\n").unwrap();
    run_git(&wt, &["add", "-A"]);
    run_git(
        &wt,
        &["commit", "-q", "-m", "change pub api on sensitive path"],
    );
    wt
}

#[test]
fn review_brief_md_includes_intent_look_here_first_and_matched_invariant() {
    let fx = Fixture::new("md");
    let sensitive_rel = "crates/foo/hooks/stop.sh";
    let plain_rel = "crates/foo/src/plain.rs";
    // A single run/decomposition with TWO tasks (avoids the second-granularity
    // run-id collision two back-to-back `state init` calls would otherwise
    // risk): t1 is the task under test, t9 is a DISTRACTOR whose violation
    // must NOT leak into t1's brief (proves the task_key filter).
    let decomp = format!(
        r#"{{"goal":"ship the hook change","tasks":[{{"id":"t1","title":"update the stop hook","touched_files":["{plain_rel}","{sensitive_rel}"],"deps":[],"class":"serial","done_criteria":"the hook is updated","kind":"feature"}},{{"id":"t9","title":"other task","touched_files":["{sensitive_rel}"],"deps":[],"class":"serial","done_criteria":"other"}}]}}"#
    );
    let rid = fx.init_run(&decomp);
    seed_sensitive_file(&fx, sensitive_rel);
    let wt = build_highrisk_worktree(&fx, sensitive_rel, "wt-t1");

    // Real post-execution diff-risk pipeline: this genuinely appends a
    // `blastguard:diffrisk-public-api` violation keyed "<rid>/t1" to the
    // isolated overwatch ledger (proven by diffrisk_record_e2e.rs).
    let out = fx.condukt(&[
        "state",
        "set",
        "--run",
        &rid,
        "--task",
        "t1",
        "--status",
        "done",
        "--worktree",
        wt.to_str().unwrap(),
        "--branch",
        "condukt/wt-t1",
    ]);
    assert!(out.status.success(), "set done failed: {out:?}");

    // Seed an UNRELATED violation on the DISTRACTOR task (same run, different
    // task_key) so the brief must filter it out.
    let wt2 = build_highrisk_worktree(&fx, sensitive_rel, "wt-t9");
    let out2 = fx.condukt(&[
        "state",
        "set",
        "--run",
        &rid,
        "--task",
        "t9",
        "--status",
        "done",
        "--worktree",
        wt2.to_str().unwrap(),
        "--branch",
        "condukt/wt-t9",
    ]);
    assert!(out2.status.success(), "set done (other) failed: {out2:?}");

    let brief = fx.condukt(&["review-brief", "--run", &rid, "--task", "t1"]);
    assert!(brief.status.success(), "review-brief failed: {brief:?}");
    let md = String::from_utf8_lossy(&brief.stdout);

    assert!(
        md.contains("update the stop hook"),
        "expected the task title in the brief; got: {md}"
    );
    assert!(
        md.contains("ship the hook change"),
        "expected the run goal in the brief; got: {md}"
    );
    assert!(
        md.contains("Look here first"),
        "expected a look-here-first section; got: {md}"
    );
    assert!(
        md.contains(sensitive_rel),
        "expected the sensitive touched file in the brief; got: {md}"
    );
    assert!(
        md.contains("diffrisk-public-api"),
        "expected the matched tripped invariant signature; got: {md}"
    );
    assert!(
        !md.contains("other task"),
        "expected the unrelated run/task's title NOT to leak into this brief; got: {md}"
    );
}

#[test]
fn review_brief_json_format_round_trips_structured_fields() {
    let fx = Fixture::new("json");
    let rel = "crates/foo/src/plain.rs";
    let decomp = format!(
        r#"{{"goal":"g","tasks":[{{"id":"t1","title":"a plain task","touched_files":["{rel}"],"deps":[],"class":"serial","done_criteria":"done","kind":"chore"}}]}}"#
    );
    let rid = fx.init_run(&decomp);

    let brief = fx.condukt(&[
        "review-brief",
        "--run",
        &rid,
        "--task",
        "t1",
        "--format",
        "json",
    ]);
    assert!(
        brief.status.success(),
        "review-brief json failed: {brief:?}"
    );
    let stdout = String::from_utf8_lossy(&brief.stdout);
    let val: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("expected valid JSON, got error {e}: {stdout}"));
    assert_eq!(val["intent"]["task_title"], "a plain task");
    assert_eq!(val["intent"]["kind"], "chore");
    assert_eq!(val["risk_tier"], "low");
    assert_eq!(val["touched_files"][0], rel);
    assert_eq!(val["look_here_first"][0], rel);
    assert!(val["tripped_invariants"].as_array().unwrap().is_empty());
}

#[test]
fn review_brief_missing_run_is_a_clean_error_not_a_panic() {
    let fx = Fixture::new("missing-run");
    let out = fx.condukt(&[
        "review-brief",
        "--run",
        "run-does-not-exist",
        "--task",
        "t1",
    ]);
    assert!(
        !out.status.success(),
        "expected a nonzero exit for a missing run"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("run-does-not-exist") || stderr.to_lowercase().contains("run"),
        "expected a clear error mentioning the missing run; got: {stderr}"
    );
}

#[test]
fn review_brief_downgrades_to_low_after_matching_ratified_precedent() {
    // A routine, multi-file, non-sensitive, invariant-clean task would
    // otherwise be Medium (declared touched_files.len() > 1). Ratify its
    // EXACT declared shape as a precedent first, then assert review-brief
    // downgrades it to `low` with a non-null `precedented` block.
    let fx = Fixture::new("precedent");
    let file_a = "crates/foo/src/a.rs";
    let file_b = "crates/foo/src/b.rs";
    let symbol = "helper";
    let decomp = format!(
        r#"{{"goal":"g","tasks":[{{"id":"t1","title":"routine multi-file refactor","touched_files":["{file_a}","{file_b}"],"target_symbols":["{symbol}"],"deps":[],"class":"serial","done_criteria":"done","kind":"chore"}}]}}"#
    );
    let rid = fx.init_run(&decomp);

    // Before ratifying: the brief is Medium (no precedent to match).
    let before = fx.condukt(&[
        "review-brief",
        "--run",
        &rid,
        "--task",
        "t1",
        "--format",
        "json",
    ]);
    assert!(before.status.success(), "review-brief failed: {before:?}");
    let before_val: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&before.stdout)).unwrap();
    assert_eq!(before_val["risk_tier"], "medium");
    assert!(before_val.get("precedented").is_none());

    // Ratify the EXACT declared shape as a precedent.
    let ratify = fx.condukt(&[
        "precedent",
        "ratify",
        "--files",
        &format!("{file_a},{file_b}"),
        "--symbols",
        symbol,
        "--note",
        "routine helper refactor",
    ]);
    assert!(
        ratify.status.success(),
        "precedent ratify failed: {ratify:?}"
    );

    // A separate `precedent list` call sees the ratified record.
    let list = fx.condukt(&["precedent", "list", "--json"]);
    assert!(list.status.success(), "precedent list failed: {list:?}");
    let list_val: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&list.stdout)).unwrap();
    assert_eq!(list_val.as_array().unwrap().len(), 1);

    // After ratifying: review-brief for the matching task downgrades to low
    // and carries a non-null precedented block.
    let after = fx.condukt(&[
        "review-brief",
        "--run",
        &rid,
        "--task",
        "t1",
        "--format",
        "json",
    ]);
    assert!(after.status.success(), "review-brief failed: {after:?}");
    let after_val: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&after.stdout)).unwrap();
    assert_eq!(after_val["risk_tier"], "low");
    assert!(
        !after_val["precedented"].is_null(),
        "expected a non-null precedented block; got: {after_val}"
    );
    assert_eq!(after_val["precedented"]["similarity"], 1.0);
}

#[test]
fn review_brief_missing_task_is_a_clean_error_not_a_panic() {
    let fx = Fixture::new("missing-task");
    let decomp = r#"{"goal":"g","tasks":[{"id":"t1","title":"a task","touched_files":[],"deps":[],"class":"serial","done_criteria":"done"}]}"#;
    let rid = fx.init_run(decomp);
    let out = fx.condukt(&["review-brief", "--run", &rid, "--task", "no-such-task"]);
    assert!(
        !out.status.success(),
        "expected a nonzero exit for a missing task"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no-such-task"),
        "expected a clear error mentioning the missing task; got: {stderr}"
    );
}
