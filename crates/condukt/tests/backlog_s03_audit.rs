// このファイルは丸ごと integration test なので unwrap/expect を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Reproduction tests for open backlog items audited in shard s03-backlog
//! (2026-10-02). Each asserts the property the ticket says is MISSING, so each
//! FAILS on the code as measured (RED observed) and is `#[ignore]`d so the
//! suite stays green. Remove the `#[ignore]` when the defect is fixed.
//! Run: `cargo test -p condukt --test backlog_s03_audit -- --ignored`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn init_git_repo(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "t@t.t"]);
    git(dir, &["config", "user.name", "t"]);
    std::fs::write(dir.join("base.txt"), "base\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", "init"]);
}

struct Fx {
    repo: PathBuf,
    home: PathBuf,
    state_dir: PathBuf,
}

impl Fx {
    fn new(tag: &str) -> Self {
        let mut base = std::env::temp_dir();
        base.push(format!("condukt-s03-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let home = base.join("home");
        let state_dir = home.join(".condukt").join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        init_git_repo(&repo);
        Self {
            repo,
            home,
            state_dir,
        }
    }

    fn run(&self, args: &[&str], path_prefix: Option<&Path>) -> std::process::Output {
        let mut c = Command::new(bin());
        c.args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("HARNESS_PROGRESS_WINDOW_SECS", "0");
        if let Some(p) = path_prefix {
            let old = std::env::var("PATH").unwrap_or_default();
            c.env("PATH", format!("{}:{}", p.display(), old));
        }
        c.output().expect("spawn condukt")
    }

    fn find_state_file(&self, name: &str) -> PathBuf {
        let mut found = Vec::new();
        for e in std::fs::read_dir(&self.state_dir).unwrap() {
            let p = e.unwrap().path().join(name);
            if p.exists() {
                found.push(p);
            }
        }
        assert_eq!(found.len(), 1, "expected one {name}, found {found:?}");
        found.remove(0)
    }
}

/// backlog c603605c: `condukt state is-claimed` answers exit 1 for BOTH "not
/// claimed" and "registry could not be read" (the unreadable case only differs
/// on stderr). A shell consumer that reads exit 1 as "free" proceeds on a
/// registry whose contents are unknown.
#[test]
fn is_claimed_distinguishes_unreadable_registry_from_not_claimed() {
    let fx = Fx::new("c603605c");
    let claim = fx.run(
        &[
            "state",
            "claim-task",
            "--run",
            "r1",
            "--session",
            "s1",
            "--hashkey",
            "abc123",
            "--title",
            "t",
        ],
        None,
    );
    assert!(
        claim.status.success(),
        "precondition: claim-task: {claim:?}"
    );

    // Control: a different, never-claimed key on a HEALTHY registry.
    let free = fx.run(&["state", "is-claimed", "--hashkey", "zzz999"], None);
    let free_code = free.status.code();
    assert_eq!(free_code, Some(1), "precondition: not-claimed is exit 1");

    // Corrupt the registry.
    let claims = fx.find_state_file("claims.json");
    std::fs::write(&claims, "{ this is not json").unwrap();
    let unreadable = fx.run(&["state", "is-claimed", "--hashkey", "zzz999"], None);
    assert!(
        String::from_utf8_lossy(&unreadable.stderr).contains("refusing to list active claims"),
        "precondition: the corrupt registry must be detected: {unreadable:?}"
    );
    assert_ne!(
        unreadable.status.code(),
        free_code,
        "is-claimed exits {:?} for an UNREADABLE registry, identical to the not-claimed exit; \
         a script keyed on the exit code cannot tell 'free' from 'could not check'",
        unreadable.status.code()
    );
}

/// backlog 8a4fe104: `backlog_pending` maps a failed `backlog list` to an empty
/// Vec, so `state execution-state` joins against "no pending tasks" and exits
/// 0 with no hint that the queue could not be read.
#[test]
#[ignore = "backlog 8a4fe104: open defect, remove ignore when fixed"]
fn execution_state_reports_an_unreadable_backlog() {
    let fx = Fx::new("8a4fe104");
    let claim = fx.run(
        &[
            "state",
            "claim-task",
            "--run",
            "r1",
            "--session",
            "s1",
            "--hashkey",
            "abc123",
            "--title",
            "t",
        ],
        None,
    );
    assert!(
        claim.status.success(),
        "precondition: claim-task: {claim:?}"
    );

    let shim = fx.home.join("shim");
    std::fs::create_dir_all(&shim).unwrap();
    let sh = shim.join("backlog");
    std::fs::write(&sh, "#!/bin/sh\necho 'queue unreadable' >&2\nexit 3\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&sh, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let out = fx.run(&["state", "execution-state"], Some(&shim));
    let said = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
    .to_lowercase();
    assert!(
        !out.status.success()
            || said.contains("unreadable")
            || said.contains("could not")
            || said.contains("undetermined"),
        "execution-state exited 0 with no sign that `backlog list` failed (exit 3): the \
         unreadable queue is rendered as 'no pending tasks'. output: {said}"
    );
}

/// backlog d65110df: `state abandon --all-stuck` exits 3 when a worktree HEAD is
/// unreadable but exit 0 when the worktree's DIRTINESS is unreadable (the veto
/// KEEPs the task and says so on stderr only). Same "cannot determine", two
/// exit statuses. Here the worktree's index is corrupt: HEAD reads, `git
/// status` fails.
#[test]
#[ignore = "backlog d65110df: open defect, remove ignore when fixed"]
fn abandon_all_stuck_dirtiness_undetermined_exits_3() {
    let fx = Fx::new("d65110df");
    let decomp = fx.repo.join("decomp.json");
    std::fs::write(
        &decomp,
        r#"{"goal":"g","tasks":[{"id":"t1","title":"x","touched_files":["a.rs"],"deps":[],"class":"serial","done_criteria":"d"}]}"#,
    )
    .unwrap();
    let init = fx.run(&["state", "init", "--file", decomp.to_str().unwrap()], None);
    assert!(init.status.success(), "state init failed: {init:?}");
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&init.stdout),
        String::from_utf8_lossy(&init.stderr)
    );
    let rid = text
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| l.starts_with("run-"))
        .expect("run id")
        .to_string();

    let wt = fx.home.join("worker-worktree");
    init_git_repo(&wt);
    let set = fx.run(
        &[
            "state",
            "set",
            "--run",
            &rid,
            "--task",
            "t1",
            "--status",
            "running",
            "--worktree",
            wt.to_str().unwrap(),
            "--branch",
            "feat/t1",
        ],
        None,
    );
    assert!(set.status.success(), "state set failed: {set:?}");

    // Backdate updated_at.
    let rs = fx.find_state_file(&format!("{rid}.json"));
    let mut v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&rs).unwrap()).unwrap();
    v["tasks"][0]["updated_at"] = serde_json::json!(1_000);
    std::fs::write(&rs, serde_json::to_string_pretty(&v).unwrap()).unwrap();

    // Corrupt the worktree's index: HEAD stays readable, `git status` fails.
    std::fs::write(wt.join(".git").join("index"), "garbage-not-an-index").unwrap();
    let probe = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(&wt)
        .output()
        .unwrap();
    assert!(
        !probe.status.success(),
        "fixture void: git status still works in the worktree"
    );
    let head = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&wt)
        .output()
        .unwrap();
    assert!(
        head.status.success(),
        "fixture void: HEAD must stay readable"
    );

    let a1 = fx.run(&["state", "abandon", "--run", &rid, "--all-stuck"], None);
    let a2 = fx.run(&["state", "abandon", "--run", &rid, "--all-stuck"], None);
    let err2 = String::from_utf8_lossy(&a2.stderr).into_owned();
    assert!(
        err2.contains("NOT abandoning")
            || String::from_utf8_lossy(&a1.stderr).contains("NOT abandoning"),
        "precondition: the dirty-veto must have fired (task KEPT); a1={a1:?} a2={a2:?}"
    );
    assert_eq!(
        a2.status.code(),
        Some(3),
        "dirtiness-undetermined KEEP exits {:?}; an unreadable HEAD exits 3. The same \
         'could not determine' must not read as a clean scan to a script. stderr: {err2}",
        a2.status.code()
    );
}

/// backlog 55fb3861: when a task's cost cannot be resolved (no agent id, no
/// `--cost-usd` set) `record-run` still forwards `--cost 0` to fugu-router, so
/// "unmeasured" is recorded as "this task cost nothing".
#[test]
#[ignore = "backlog 55fb3861: open defect, remove ignore when fixed"]
fn record_run_does_not_fabricate_zero_cost_for_an_unmeasured_task() {
    let fx = Fx::new("55fb3861");
    let shim = fx.home.join("shim");
    std::fs::create_dir_all(&shim).unwrap();
    let log = fx.home.join("fugu.log");
    let sh = shim.join("fugu-router");
    std::fs::write(
        &sh,
        format!(
            "#!/bin/sh\necho \"$@\" >> '{}'\necho fp123\nexit 0\n",
            log.display()
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&sh, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let decomp = fx.repo.join("decomp.json");
    std::fs::write(
        &decomp,
        r#"{"goal":"g","tasks":[{"id":"t1","title":"x","touched_files":["a.rs"],"deps":[],"class":"serial","done_criteria":"d"}]}"#,
    )
    .unwrap();
    let init = fx.run(&["state", "init", "--file", decomp.to_str().unwrap()], None);
    assert!(init.status.success(), "state init failed: {init:?}");
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&init.stdout),
        String::from_utf8_lossy(&init.stderr)
    );
    let rid = text
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| l.starts_with("run-"))
        .expect("run id")
        .to_string();
    for st in ["running", "failed"] {
        let o = fx.run(
            &[
                "state", "set", "--run", &rid, "--task", "t1", "--status", st,
            ],
            None,
        );
        assert!(o.status.success(), "state set {st} failed: {o:?}");
    }
    let rec = fx.run(&["state", "record-run", "--run", &rid], Some(&shim));
    assert!(rec.status.success(), "precondition: record-run: {rec:?}");
    let calls = std::fs::read_to_string(&log).expect("fugu-router was invoked");
    let record_line = calls
        .lines()
        .find(|l| l.starts_with("record "))
        .unwrap_or_else(|| panic!("no `record` call in {calls:?}"));
    assert!(
        !record_line.contains("--cost 0"),
        "an unresolved cost was recorded as 0: {record_line}"
    );
}
