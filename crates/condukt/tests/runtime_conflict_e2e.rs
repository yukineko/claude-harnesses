//! End-to-end coverage for mid-flight runtime-conflict detection + merge-hold
//! (design 625aa170 — decision A). Two in-flight worktrees each edit the SAME
//! file that NEITHER declared in its `touched_files` (so condukt's schedule-time
//! parallel-safety was structurally blind to it). When both transition to
//! `done`, the second `done` detects the actual-diff overlap: exactly ONE
//! runtime-conflict event names the shared file, and a merge-hold (an open
//! `[merge-conflict]` RuntimeOverlap entry) is enqueued into the consensus
//! review surface. A single task with no peer records NO event / NO hold.
//!
//! Binary spawned against an isolated HOME so condukt state (`<home>/.condukt`)
//! and the overwatch registry (`<home>/.overwatch`) both land in a throwaway dir.

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
        base.push(format!("condukt-runtimeconflict-{pid}-{tag}"));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        run_git(&repo, &["init", "-q", "-b", "main"]);
        run_git(&repo, &["config", "user.email", "t@t.t"]);
        run_git(&repo, &["config", "user.name", "t"]);
        // Seed the shared file on main so both branches fork from a common base.
        std::fs::write(repo.join("shared.rs"), "fn base() {}\n").unwrap();
        std::fs::write(repo.join("readme.md"), "seed\n").unwrap();
        run_git(&repo, &["add", "-A"]);
        run_git(&repo, &["commit", "-q", "-m", "seed"]);
        Self { base, repo, home }
    }

    fn condukt(&self, args: &[&str]) -> std::process::Output {
        Command::new(bin())
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_SESSION_ID", "sess-rt")
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

    fn read_store(&self, suffix: &str) -> Option<String> {
        find_by_suffix(&self.home.join(".overwatch"), suffix)
            .and_then(|p| std::fs::read_to_string(p).ok())
    }

    /// Build a worktree/branch that edits `shared.rs` (UNDECLARED) — the file is
    /// not in the task's declared touched_files, so only the ACTUAL-diff check
    /// can see it. Returns the worktree path.
    fn edit_shared_worktree(&self, task_id: &str, content: &str) -> PathBuf {
        let wt = self.base.join(format!("wt-{task_id}"));
        run_git(
            &self.repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                &format!("condukt/{task_id}"),
                wt.to_str().unwrap(),
                "main",
            ],
        );
        std::fs::write(wt.join("shared.rs"), content).unwrap();
        run_git(&wt, &["add", "-A"]);
        run_git(
            &wt,
            &["commit", "-q", "-m", &format!("edit shared on {task_id}")],
        );
        wt
    }

    fn set_done(&self, rid: &str, task_id: &str, wt: &Path) {
        let out = self.condukt(&[
            "state",
            "set",
            "--run",
            rid,
            "--task",
            task_id,
            "--status",
            "done",
            "--worktree",
            wt.to_str().unwrap(),
            "--branch",
            &format!("condukt/{task_id}"),
        ]);
        assert!(out.status.success(), "set done {task_id} failed: {out:?}");
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

#[test]
fn two_worktrees_editing_undeclared_shared_file_detect_overlap_and_set_hold() {
    let fx = Fixture::new("overlap");
    // Both tasks DECLARE only their own private file — shared.rs is UNDECLARED.
    let decomp = r#"{"goal":"g","tasks":[
        {"id":"t1","title":"work A","touched_files":["a1.rs"],"deps":[],"class":"parallel","done_criteria":"done"},
        {"id":"t2","title":"work B","touched_files":["b1.rs"],"deps":[],"class":"parallel","done_criteria":"done"}
    ]}"#;
    let rid = fx.init_run(decomp);

    // t1 finishes first: records its changeset, NO peer yet -> no conflict/hold.
    let wt1 = fx.edit_shared_worktree("t1", "fn base() {}\nfn a_change() {}\n");
    fx.set_done(&rid, "t1", &wt1);
    assert!(
        fx.read_store("runtime_conflicts.jsonl").is_none(),
        "first task must not record any overlap"
    );

    // t2 finishes: its actual diff overlaps t1 on shared.rs (undeclared) -> one event + hold.
    let wt2 = fx.edit_shared_worktree("t2", "fn base() {}\nfn b_change() {}\n");
    fx.set_done(&rid, "t2", &wt2);

    let conflicts = fx
        .read_store("runtime_conflicts.jsonl")
        .expect("a runtime_conflicts.jsonl must exist after the overlapping done");
    let lines: Vec<&str> = conflicts.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(
        lines.len(),
        1,
        "exactly one overlap event; got: {conflicts}"
    );
    assert!(
        lines[0].contains("shared.rs"),
        "the event must name the shared file: {conflicts}"
    );
    assert!(
        lines[0].contains(&format!("{rid}/t2")) && lines[0].contains(&format!("{rid}/t1")),
        "the event must name both tasks: {conflicts}"
    );

    // The merge-hold: an OPEN RuntimeOverlap merge-conflict entry for t2.
    let holds = fx
        .read_store("merge_conflicts.jsonl")
        .expect("a merge_conflicts.jsonl hold must be enqueued");
    assert!(
        holds.contains("runtime-overlap"),
        "hold origin must be runtime-overlap: {holds}"
    );
    assert!(
        holds.contains(&format!("{rid}/t2/runtime-overlap")),
        "hold id keyed by task: {holds}"
    );
    assert!(
        holds.contains("condukt/t2"),
        "hold names the held branch: {holds}"
    );
}

#[test]
fn single_task_records_changeset_but_no_conflict_or_hold() {
    let fx = Fixture::new("single");
    let decomp = r#"{"goal":"g","tasks":[
        {"id":"t1","title":"solo","touched_files":["a1.rs"],"deps":[],"class":"serial","done_criteria":"done"}
    ]}"#;
    let rid = fx.init_run(decomp);
    let wt1 = fx.edit_shared_worktree("t1", "fn base() {}\nfn solo() {}\n");
    fx.set_done(&rid, "t1", &wt1);

    // The changeset registry exists, but NO overlap event / NO hold.
    assert!(
        fx.read_store("active_changesets.json").is_some(),
        "the changeset must be recorded"
    );
    assert!(
        fx.read_store("runtime_conflicts.jsonl").is_none(),
        "a lone task must not produce a conflict event"
    );
    assert!(
        fx.read_store("merge_conflicts.jsonl").is_none(),
        "a lone task must not set a hold"
    );
}
