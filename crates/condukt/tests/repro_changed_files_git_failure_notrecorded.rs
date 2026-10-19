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
#[ignore = "backlog: changed_files git failure maps to NotRecorded, indistinguishable from an empty diff (no hold, silent)"]
fn git_failure_listing_changed_files_is_not_silent_norecord() {
    let fx = Fixture::new("gitfail");
    let decomp = r#"{"goal":"g","tasks":[
        {"id":"t1","title":"solo","touched_files":["a1.rs"],"deps":[],"class":"serial","done_criteria":"done"}
    ]}"#;
    let rid = fx.init_run(decomp);
    let wt = fx.edit_shared_worktree("t1", "fn base() {}\nfn solo() {}\n");
    // Make `git diff` fail on every fallback while merge-base/rev-parse still
    // work: delete the loose TREE object of the worktree's HEAD commit.
    // Delete the loose TREE objects of both HEAD and the base (main) commit so
    // all three `git diff` fallbacks fail; commit objects (merge-base) survive.
    for rev in ["HEAD^{tree}", "main^{tree}"] {
        let tree = String::from_utf8(
            Command::new("git")
                .args(["rev-parse", rev])
                .current_dir(&wt)
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        let tree = tree.trim();
        let obj = fx
            .repo
            .join(".git/objects")
            .join(&tree[..2])
            .join(&tree[2..]);
        assert!(obj.exists(), "loose tree object expected at {obj:?}");
        std::fs::remove_file(&obj).unwrap();
    }
    assert!(
        !Command::new("git")
            .args(["diff", "--name-only", "HEAD"])
            .current_dir(&wt)
            .output()
            .unwrap()
            .status
            .success(),
        "precondition: git diff must fail in the worktree"
    );

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
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let hold = fx.read_store("merge_conflicts.jsonl").is_some();
    let recorded = fx.read_store("active_changesets.json").is_some();
    // A truly empty diff produces none of these signals. A git failure must
    // produce at least one (hold, recorded, non-zero exit, or a stderr naming overlap).
    let signalled =
        hold || recorded || !out.status.success() || stderr.to_lowercase().contains("overlap");
    assert!(
        signalled,
        "git could not list the changed files, yet condukt reported nothing and set no hold \
         (indistinguishable from an empty diff). exit={:?} stderr={stderr}",
        out.status
    );
}
