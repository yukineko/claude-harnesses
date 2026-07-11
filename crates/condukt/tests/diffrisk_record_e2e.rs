//! End-to-end coverage for the post-execution diff-risk recording (finding 4 /
//! WorkItem-A): when a worker's task transitions to `done`, condukt diffs the
//! task's worktree against the base branch, feeds the REAL diff to blastguard's
//! `classify_diff`, and — on a High-risk verdict (a public-API change on a
//! sensitive path) — records ONE `ViolationSource::Blastguard` event with the
//! `blastguard:diffrisk-public-api` signature to the overwatch violation
//! registry.
//!
//! This is the proof that the public-API risk signal, which is DEAD at the
//! pre-execution call sites (they pass an empty diff), now fires once a real
//! diff exists post-execution. The binary is spawned against an isolated
//! HOME so both condukt's state (`<home>/.condukt`) and overwatch's violation
//! ledger (`<home>/.overwatch`) land in a throwaway dir.

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
        base.push(format!("condukt-diffrisk-{pid}-{tag}"));
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

    /// The overwatch violation ledger for this repo (path is under the isolated
    /// HOME/.overwatch/<project-key>/overwatch/violations.jsonl).
    fn violations(&self) -> Option<String> {
        find_by_suffix(&self.home.join(".overwatch"), "violations.jsonl")
            .and_then(|p| std::fs::read_to_string(p).ok())
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

fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git");
    assert!(out.status.success(), "git {args:?} failed: {out:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
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

fn find_by_suffix(root: &Path, suffix: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(root).ok()?;
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Some(found) = find_by_suffix(&p, suffix) {
                return Some(found);
            }
        } else if p.to_string_lossy().ends_with(suffix) {
            return Some(p);
        }
    }
    None
}

/// Build a worktree branch whose diff-vs-main changes a `pub fn` signature on a
/// SENSITIVE path (a `hooks/` file), so blastguard's `classify_diff` grades it
/// High (public-API signal AND sensitive-path signal both fire). Returns the
/// worktree path.
fn build_highrisk_worktree(fx: &Fixture, sensitive_rel: &str) -> PathBuf {
    // Seed main with an initial version of the sensitive file (a `pub fn`).
    let file = fx.repo.join(sensitive_rel);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "pub fn old_api(x: i32) {}\n").unwrap();
    run_git(&fx.repo, &["add", "-A"]);
    run_git(&fx.repo, &["commit", "-q", "-m", "seed"]);

    // Create a task worktree/branch off main and change the pub fn signature.
    let wt = fx.base.join("wt-t1");
    run_git(
        &fx.repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "condukt/t1",
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
fn done_transition_with_public_api_change_on_sensitive_path_records_violation() {
    let fx = Fixture::new("fires");
    let sensitive_rel = "crates/foo/hooks/stop.sh";
    // The task's touched_files include the sensitive path so classify_diff's
    // path signal fires (WorkItem-D translated globs match `**/hooks/**`).
    let decomp = format!(
        r#"{{"goal":"g","tasks":[{{"id":"t1","title":"change a hook api","touched_files":["{sensitive_rel}"],"deps":[],"class":"serial","done_criteria":"the hook is updated"}}]}}"#
    );
    let rid = fx.init_run(&decomp);
    let wt = build_highrisk_worktree(&fx, sensitive_rel);

    // Mark the task done, pointing at the finished worktree/branch. This is the
    // post-worker/pre-merge edge where the diff-risk hook fires.
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
        "condukt/t1",
    ]);
    assert!(out.status.success(), "set done failed: {out:?}");

    let ledger = fx
        .violations()
        .expect("a violations.jsonl must be written for a High-risk done task");
    assert!(
        ledger.contains("\"blastguard:diffrisk-public-api\""),
        "expected a blastguard diffrisk-public-api violation signature; got: {ledger}"
    );
    assert!(
        ledger.contains("\"source\":\"blastguard\""),
        "expected ViolationSource::Blastguard; got: {ledger}"
    );
    assert!(
        ledger.contains(&format!("{rid}/t1")),
        "expected the task_key to scope run/task; got: {ledger}"
    );
    let _ = git_out(&fx.repo, &["rev-parse", "HEAD"]); // sanity: repo is a real git tree
}

/// Seed a repo where a changed **public** symbol on a NON-sensitive path is
/// referenced from `n_callers` distinct `.rs` files in the worktree, so only
/// the caller blast-radius signal (not the sensitive-path base) escalates the
/// diff to High. Returns the worktree path.
///
/// `api_rel` (the changed public symbol) is seeded on main as `pub fn old_api`
/// and the branch renames it to `pub fn new_api` — both are "changed symbols".
/// The caller files (seeded on main, so present in the worktree, UNCHANGED on
/// the branch) each reference `old_api(` in call position, giving the removed
/// symbol `old_api` a blast radius of `n_callers` sites.
fn build_caller_heavy_worktree(fx: &Fixture, api_rel: &str, n_callers: usize) -> PathBuf {
    let api = fx.repo.join(api_rel);
    std::fs::create_dir_all(api.parent().unwrap()).unwrap();
    std::fs::write(&api, "pub fn old_api(x: i32) {}\n").unwrap();

    // Caller files on a NON-sensitive path, each calling `old_api(...)`.
    let callers_dir = "crates/foo/src/callers";
    for i in 0..n_callers {
        let rel = format!("{callers_dir}/c{i}.rs");
        let p = fx.repo.join(&rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, format!("fn uses_{i}() {{\n    old_api({i});\n}}\n")).unwrap();
    }
    run_git(&fx.repo, &["add", "-A"]);
    run_git(&fx.repo, &["commit", "-q", "-m", "seed api + callers"]);

    let wt = fx.base.join("wt-t1");
    run_git(
        &fx.repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "condukt/t1",
            wt.to_str().unwrap(),
            "main",
        ],
    );
    // Rename the public symbol (public-API change) on the NON-sensitive path.
    // The caller files are left untouched, so `old_api` now has no declaration
    // in the worktree yet is still referenced from `n_callers` sites.
    std::fs::write(wt.join(api_rel), "pub fn new_api(x: i32, y: i32) {}\n").unwrap();
    run_git(&wt, &["add", "-A"]);
    run_git(
        &wt,
        &["commit", "-q", "-m", "rename pub api on non-sensitive path"],
    );
    wt
}

#[test]
fn done_transition_with_caller_blast_radius_records_callgraph_violation() {
    // A public-API change on a NON-sensitive path: base is Medium (public-API
    // signal only, no sensitive path), so the ORIGINAL diffrisk-public-api rule
    // does NOT fire. But the changed symbol is called from >= threshold (5)
    // sites, so the CALLER blast-radius signal escalates the diff to High and a
    // distinct `diffrisk-callgraph` violation is recorded.
    let fx = Fixture::new("callgraph-fires");
    let api_rel = "crates/foo/src/api.rs"; // NON-sensitive path
    let decomp = format!(
        r#"{{"goal":"g","tasks":[{{"id":"t1","title":"rename a public api","touched_files":["{api_rel}"],"deps":[],"class":"serial","done_criteria":"the api is renamed"}}]}}"#
    );
    let rid = fx.init_run(&decomp);
    let wt = build_caller_heavy_worktree(&fx, api_rel, 5);

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
        "condukt/t1",
    ]);
    assert!(out.status.success(), "set done failed: {out:?}");

    let ledger = fx
        .violations()
        .expect("a violations.jsonl must be written for a caller-heavy done task");
    assert!(
        ledger.contains("\"blastguard:diffrisk-callgraph\""),
        "expected a blastguard diffrisk-callgraph violation signature; got: {ledger}"
    );
    // The base (sensitive-path) rule must NOT fire on this non-sensitive path.
    assert!(
        !ledger.contains("diffrisk-public-api"),
        "the non-sensitive caller case must not record the base public-API rule; got: {ledger}"
    );
    assert!(
        ledger.contains(&format!("{rid}/t1")),
        "expected the task_key to scope run/task; got: {ledger}"
    );
}

#[test]
fn done_transition_public_api_change_with_zero_callers_records_nothing() {
    // Same public-API change on a NON-sensitive path, but with NO callers in
    // the worktree corpus: base is Medium and the caller signal is Low, so the
    // diff never reaches High → nothing is recorded.
    let fx = Fixture::new("callgraph-silent");
    let api_rel = "crates/foo/src/api.rs"; // NON-sensitive path
    let decomp = format!(
        r#"{{"goal":"g","tasks":[{{"id":"t1","title":"rename a public api","touched_files":["{api_rel}"],"deps":[],"class":"serial","done_criteria":"the api is renamed"}}]}}"#
    );
    let rid = fx.init_run(&decomp);
    // Zero callers: build the caller-heavy worktree with n_callers = 0.
    let wt = build_caller_heavy_worktree(&fx, api_rel, 0);

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
        "condukt/t1",
    ]);
    assert!(out.status.success(), "set done failed: {out:?}");

    // No High-risk verdict → no diffrisk signature of EITHER kind.
    if let Some(ledger) = fx.violations() {
        assert!(
            !ledger.contains("diffrisk-callgraph") && !ledger.contains("diffrisk-public-api"),
            "a caller-less public-API change on a non-sensitive path must not record a diffrisk violation; got: {ledger}"
        );
    }
}

#[test]
fn done_transition_with_only_private_change_records_nothing() {
    // A change that touches NEITHER a public symbol NOR a sensitive path stays
    // Low/Medium and must NOT be recorded (observational: only High fires).
    let fx = Fixture::new("silent");
    let rel = "crates/foo/src/util.rs";
    let decomp = format!(
        r#"{{"goal":"g","tasks":[{{"id":"t1","title":"private tweak","touched_files":["{rel}"],"deps":[],"class":"serial","done_criteria":"tweaked"}}]}}"#
    );
    let rid = fx.init_run(&decomp);

    // Seed + change a PRIVATE fn on a NON-sensitive path.
    let file = fx.repo.join(rel);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "fn helper() {}\n").unwrap();
    run_git(&fx.repo, &["add", "-A"]);
    run_git(&fx.repo, &["commit", "-q", "-m", "seed"]);
    let wt = fx.base.join("wt-t1");
    run_git(
        &fx.repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "condukt/t1",
            wt.to_str().unwrap(),
            "main",
        ],
    );
    std::fs::write(wt.join(rel), "fn helper2() {}\n").unwrap();
    run_git(&wt, &["add", "-A"]);
    run_git(&wt, &["commit", "-q", "-m", "private change"]);

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
        "condukt/t1",
    ]);
    assert!(out.status.success(), "set done failed: {out:?}");

    // No High-risk verdict → no violation ledger written (or at least no
    // diffrisk signature in it).
    if let Some(ledger) = fx.violations() {
        assert!(
            !ledger.contains("diffrisk-public-api"),
            "a private/non-sensitive change must not record a diffrisk violation; got: {ledger}"
        );
    }
}
