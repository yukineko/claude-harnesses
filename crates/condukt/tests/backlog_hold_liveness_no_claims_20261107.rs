// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (2026-11-07): a runtime-overlap hold placed by a live run that holds zero claims.
//! Fixture copied from repro_overlap_hold_empty_branch.rs.
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
        harness_core::git_env::scrub(&mut Command::new(bin()))
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
    let ok = harness_core::git_env::scrub(&mut Command::new("git"))
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
#[ignore = "backlog 4fa94cba: RED repro — runtime-overlap hold placed by a live run with zero claims is ignored and the merge proceeds"]
fn runtime_overlap_hold_from_live_run_with_zero_claims_still_blocks_merge() {
    let fx = Fixture::new("holdliveness");
    // t3 stays pending, so the run-state is non-terminal (the run is live).
    // No task is ever set `running`, so the run never holds any claim.
    let decomp = r#"{"goal":"g","tasks":[
        {"id":"t1","title":"A","touched_files":["a1.rs"],"deps":[],"class":"parallel","done_criteria":"done"},
        {"id":"t2","title":"B","touched_files":["b1.rs"],"deps":[],"class":"parallel","done_criteria":"done"},
        {"id":"t3","title":"C","touched_files":["c1.rs"],"deps":[],"class":"parallel","done_criteria":"done"}
    ]}"#;
    let rid = fx.init_run(decomp);
    let wt1 = fx.edit_shared_worktree("t1", "fn base() {}\nfn a() {}\n");
    fx.set_done(&rid, "t1", &wt1);
    let wt2 = fx.edit_shared_worktree("t2", "fn base() {}\nfn b() {}\n");
    fx.set_done(&rid, "t2", &wt2);
    let holds = fx
        .read_store("merge_conflicts.jsonl")
        .expect("overlap hold must be enqueued");
    assert!(
        holds.contains(&format!("{rid}/t2/runtime-overlap")),
        "precondition: t2 hold placed by {rid}: {holds}"
    );
    // Precondition: the placer run holds no claim at all.
    let claims = find_by_suffix(&fx.home, "claims.json")
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    assert!(
        !claims.contains(&rid),
        "precondition: run {rid} must hold zero claims: {claims}"
    );
    // The merge gate also requires the Condukt-Task trailer on every commit.
    run_git(
        &wt2,
        &[
            "commit",
            "-q",
            "--amend",
            "-m",
            &format!("edit shared on t2\n\nCondukt-Task: {rid}/t2"),
        ],
    );
    // Record the creation base the merge gate requires (otherwise it refuses for
    // an unrelated reason, merge-base-unrecorded).
    let base = Command::new("git")
        .args(["rev-parse", "main"])
        .current_dir(&fx.repo)
        .output()
        .unwrap();
    let base = String::from_utf8_lossy(&base.stdout).trim().to_string();
    run_git(
        &fx.repo,
        &["config", "branch.condukt/t2.conduktBase", &base],
    );
    let out = fx.condukt(&[
        "worktree",
        "merge",
        "--branch",
        "condukt/t2",
        "--run",
        &rid,
        "--task",
        "t2",
    ]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("HELD"),
        "the hold was placed by live run {rid} (non-terminal run-state, zero claims); \
         run_liveness read the empty claim set as Dead and the merge proceeded: {out:?}"
    );
    let log = Command::new("git")
        .args(["log", "--oneline", "main"])
        .current_dir(&fx.repo)
        .output()
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&log.stdout).contains("edit shared on t2"),
        "t2 must NOT have landed on main while its hold is open"
    );
}
