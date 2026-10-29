// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Repro (see per-test docs). Fixture copied from runtime_conflict_e2e.rs.
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
fn overlap_hold_for_task_without_recorded_branch_has_empty_branch() {
    let fx = Fixture::new("nobranch");
    let decomp = r#"{"goal":"g","tasks":[
        {"id":"t1","title":"A","touched_files":["a1.rs"],"deps":[],"class":"parallel","done_criteria":"done"},
        {"id":"t2","title":"B","touched_files":["b1.rs"],"deps":[],"class":"parallel","done_criteria":"done"}
    ]}"#;
    let rid = fx.init_run(decomp);
    let wt1 = fx.edit_shared_worktree("t1", "fn base() {}\nfn a() {}\n");
    fx.set_done(&rid, "t1", &wt1);
    let wt2 = fx.edit_shared_worktree("t2", "fn base() {}\nfn b() {}\n");
    // `state set --status done --worktree <wt>` WITHOUT --branch: branch stays None.
    let out = fx.condukt(&[
        "state",
        "set",
        "--run",
        &rid,
        "--task",
        "t2",
        "--status",
        "done",
        "--worktree",
        wt2.to_str().unwrap(),
    ]);
    assert!(out.status.success(), "set done failed: {out:?}");
    let holds = fx
        .read_store("merge_conflicts.jsonl")
        .expect("overlap hold must be enqueued");
    assert!(
        holds.contains(&format!("{rid}/t2/runtime-overlap")),
        "t2 hold expected: {holds}"
    );
    assert!(
        !holds.contains("\"branch\":\"\""),
        "the hold was written with an EMPTY branch; `worktree merge --branch condukt/t2` matches \
         holds by e.branch == branch and can never find it: {holds}"
    );
}
