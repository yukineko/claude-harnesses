//! End-to-end coverage for `condukt gate check` — the deterministic gate-exec
//! decision (auto-execute a clearly-safe gated task vs escalate to a human).
//! Spawns the built binary against an isolated temp HOME/state dir so it
//! exercises the real CLI: decomposition loading, the graded risk classifier,
//! the autonomy predicate, the checkpoint-then-journal side effects, and the
//! process exit-code contract callers branch on. Fails before the `gate`
//! subcommand exists (unrecognized subcommand -> clap exit 2) and passes once it
//! is wired = a genuine Fail->Pass reproduction oracle for the wiring task.

use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

/// A throwaway git repo + an isolated HOME so the run's state, checkpoints and
/// journals land under `<home>/.condukt/state` and never touch the developer's
/// real store (condukt derives its base dir from `$HOME/.condukt`).
struct Fixture {
    repo: PathBuf,
    home: PathBuf,
    state_dir: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let pid = std::process::id();
        let mut base = std::env::temp_dir();
        base.push(format!("condukt-gate-cli-{pid}-{tag}"));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let home = base.join("home");
        let state_dir = home.join(".condukt").join("state");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&state_dir).unwrap();
        // Minimal git repo so `worktree::toplevel` resolves without error.
        run_git(&repo, &["init", "-q"]);
        run_git(&repo, &["config", "user.email", "t@t.t"]);
        run_git(&repo, &["config", "user.name", "t"]);
        Self {
            repo,
            home,
            state_dir,
        }
    }

    /// Run the binary with the isolated HOME. `autonomous` toggles
    /// `CONDUKT_AUTONOMOUS` (the env the autonomy predicate reads).
    fn condukt(&self, args: &[&str], autonomous: bool) -> std::process::Output {
        Command::new(bin())
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("CONDUKT_AUTONOMOUS", if autonomous { "1" } else { "0" })
            .output()
            .expect("spawn condukt")
    }

    fn init_run(&self, decomp_json: &str) -> String {
        let p = self.repo.join("decomp.json");
        std::fs::write(&p, decomp_json).unwrap();
        let out = self.condukt(&["state", "init", "--file", p.to_str().unwrap()], false);
        assert!(out.status.success(), "init failed: {out:?}");
        run_id_from(&out)
    }

    fn find_state_file(&self, suffix: &str) -> Option<PathBuf> {
        find_by_suffix(&self.state_dir, suffix)
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

/// Extract the run id from `state init`'s output (the `run-` line).
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

/// Recursively find a file whose path ends with `suffix` (the project-key dir is
/// a hash we don't want to recompute in the test).
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

/// A decomposition with one Low-risk, reversible task (ordinary local work — no
/// deploy/push/destructive signal → classifier returns Low + reversible).
fn safe_decomp() -> &'static str {
    r#"{"goal":"g","tasks":[{"id":"t1","title":"rename a local helper function","touched_files":["src/util.rs"],"deps":[],"class":"serial","done_criteria":"the helper is renamed and unit tests pass"}]}"#
}

/// A decomposition with one High-risk irreversible task (an outward release/push
/// → classifier returns High + not reversible → always escalates).
fn risky_decomp() -> &'static str {
    r#"{"goal":"g","tasks":[{"id":"t1","title":"deploy the release and git push to production","touched_files":["deploy.sh"],"deps":[],"class":"gated","done_criteria":"the release is pushed to production"}]}"#
}

#[test]
fn low_risk_reversible_autonomous_auto_execs_checkpoints_and_journals() {
    let fx = Fixture::new("autoexec");
    let rid = fx.init_run(safe_decomp());

    let out = fx.condukt(&["gate", "check", "--run", &rid, "--task", "t1"], true);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "Low+reversible under autonomous ON must auto-exec (exit 0); out={out:?}"
    );
    assert!(
        stdout.contains("\"verdict\": \"auto_exec\""),
        "expected auto_exec verdict; got {stdout}"
    );
    assert!(
        stdout.contains("\"policy_is_auto\": true"),
        "autonomous ON must yield policy_is_auto true; got {stdout}"
    );

    // A checkpoint was written FIRST (the run is recoverable before it runs).
    assert!(
        fx.find_state_file(&format!("{rid}.checkpoints.json"))
            .is_some(),
        "auto-exec must write a run checkpoint"
    );
    // The decision was journaled.
    let journal = fx
        .find_state_file(&format!("{rid}.gate-exec-log.jsonl"))
        .expect("auto-exec must journal the decision");
    let jtext = std::fs::read_to_string(&journal).unwrap();
    assert!(
        jtext.contains("\"verdict\":\"auto_exec\""),
        "journal must record the auto_exec decision: {jtext}"
    );
}

#[test]
fn low_risk_reversible_non_autonomous_escalates_nonzero() {
    let fx = Fixture::new("nonauto");
    let rid = fx.init_run(safe_decomp());

    // SAME task, autonomous OFF → policy not auto → escalate (backward-compat).
    let out = fx.condukt(&["gate", "check", "--run", &rid, "--task", "t1"], false);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(1),
        "autonomous OFF must escalate (nonzero exit); out={out:?}"
    );
    assert!(
        stdout.contains("\"verdict\": \"escalate\""),
        "expected escalate verdict; got {stdout}"
    );
    assert!(
        stdout.contains("\"policy_is_auto\": false"),
        "autonomous OFF must yield policy_is_auto false; got {stdout}"
    );
    // Escalate must NOT checkpoint (only auto-exec is a recoverable action).
    assert!(
        fx.find_state_file(&format!("{rid}.checkpoints.json"))
            .is_none(),
        "escalate must not write a checkpoint"
    );
    // ...but it IS journaled.
    assert!(
        fx.find_state_file(&format!("{rid}.gate-exec-log.jsonl"))
            .is_some(),
        "escalate must still journal the decision"
    );
}

#[test]
fn high_risk_irreversible_escalates_even_when_autonomous() {
    let fx = Fixture::new("risky");
    let rid = fx.init_run(risky_decomp());

    // Even under autonomous ON, a High-risk irreversible action always escalates.
    let out = fx.condukt(&["gate", "check", "--run", &rid, "--task", "t1"], true);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(1),
        "High-risk irreversible must escalate even under autonomous; out={out:?}"
    );
    assert!(
        stdout.contains("\"verdict\": \"escalate\""),
        "expected escalate verdict; got {stdout}"
    );
    assert!(
        stdout.contains("\"risk\": \"high\""),
        "expected high risk signal; got {stdout}"
    );
    // No checkpoint for an escalated action.
    assert!(
        fx.find_state_file(&format!("{rid}.checkpoints.json"))
            .is_none(),
        "escalate must not write a checkpoint"
    );
}

#[test]
fn missing_task_fails_soft_to_escalate_nonzero() {
    let fx = Fixture::new("missing-task");
    let rid = fx.init_run(safe_decomp());

    // A task id that isn't in the decomposition must fail soft (no panic).
    let out = fx.condukt(&["gate", "check", "--run", &rid, "--task", "nope"], true);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(1),
        "missing task must fail soft to escalate; out={out:?}"
    );
    assert!(
        stdout.contains("\"verdict\": \"escalate\""),
        "expected escalate verdict; got {stdout}"
    );
    assert!(
        fx.find_state_file(&format!("{rid}.checkpoints.json"))
            .is_none(),
        "a fail-soft escalate must not checkpoint"
    );
}

#[test]
fn missing_run_fails_soft_to_escalate_nonzero() {
    let fx = Fixture::new("missing-run");
    // No run initialised at all → decomposition load fails → escalate, no panic.
    let out = fx.condukt(
        &[
            "gate",
            "check",
            "--run",
            "run-does-not-exist",
            "--task",
            "t1",
        ],
        true,
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(1),
        "missing run must fail soft to escalate; out={out:?}"
    );
    assert!(
        stdout.contains("\"verdict\": \"escalate\""),
        "expected escalate verdict; got {stdout}"
    );
}
