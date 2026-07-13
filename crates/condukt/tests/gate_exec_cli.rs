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

    /// Path to overwatch's review_findings.jsonl for this fixture, computed
    /// WITHOUT touching the parent test process's own `$HOME` (that would race
    /// with other tests running in parallel in this binary). Mirrors
    /// `overwatch::store`'s `storage_root`: `$HOME/.overwatch/<project-key>/overwatch/`.
    /// `harness_core::projkey` is pure (no env), so this is safe to call in-process.
    fn review_findings_path(&self) -> PathBuf {
        let root = harness_core::projkey::repo_root(&self.repo);
        let key = harness_core::projkey::project_key(&root);
        self.home
            .join(".overwatch")
            .join(key)
            .join("overwatch")
            .join("review_findings.jsonl")
    }

    /// Read back all recorded review findings (empty vec if the file is absent).
    fn review_findings(&self) -> Vec<overwatch::review_finding::ReviewFinding> {
        match std::fs::read_to_string(self.review_findings_path()) {
            Ok(txt) => txt
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(|l| serde_json::from_str(l).expect("valid ReviewFinding JSON line"))
                .collect(),
            Err(_) => Vec::new(),
        }
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

// ── overwatch review-finding wiring (Escalate records, AutoExec doesn't) ──────

#[test]
fn escalate_records_exactly_one_review_finding() {
    let fx = Fixture::new("escalate-finding");
    let rid = fx.init_run(safe_decomp());

    // No finding before the gate has ever run.
    assert!(
        fx.review_findings().is_empty(),
        "no finding should exist before any gate check"
    );

    // autonomous OFF => Low+reversible still escalates (policy not auto).
    let out = fx.condukt(&["gate", "check", "--run", &rid, "--task", "t1"], false);
    assert_eq!(out.status.code(), Some(1), "escalate must exit 1");

    let findings = fx.review_findings();
    assert_eq!(
        findings.len(),
        1,
        "escalate must record exactly one review finding: {findings:?}"
    );
    let f = &findings[0];
    assert_eq!(f.finding_id, format!("gate-exec:{rid}:t1"));
    assert_eq!(f.source, "condukt-gate");
    assert_eq!(
        f.severity.as_deref(),
        Some("medium"),
        "Low risk must map to medium severity: {f:?}"
    );
    assert!(
        f.summary.contains("t1"),
        "summary should name the gated task: {}",
        f.summary
    );
}

#[test]
fn escalate_high_risk_records_high_severity_finding() {
    let fx = Fixture::new("escalate-high-finding");
    let rid = fx.init_run(risky_decomp());

    // High-risk irreversible always escalates, even under autonomous ON.
    let out = fx.condukt(&["gate", "check", "--run", &rid, "--task", "t1"], true);
    assert_eq!(out.status.code(), Some(1), "escalate must exit 1");

    let findings = fx.review_findings();
    assert_eq!(
        findings.len(),
        1,
        "expected exactly one finding: {findings:?}"
    );
    assert_eq!(
        findings[0].severity.as_deref(),
        Some("high"),
        "High risk must map to high severity: {:?}",
        findings[0]
    );
}

#[test]
fn autoexec_records_no_review_finding() {
    let fx = Fixture::new("autoexec-no-finding");
    let rid = fx.init_run(safe_decomp());

    // Low+reversible+autonomous ON => auto-exec, no escalation at all.
    let out = fx.condukt(&["gate", "check", "--run", &rid, "--task", "t1"], true);
    assert_eq!(out.status.code(), Some(0), "auto-exec must exit 0");

    assert!(
        fx.review_findings().is_empty(),
        "auto-exec must record NO review finding: {:?}",
        fx.review_findings()
    );
}

#[test]
fn repeated_escalate_of_the_same_gate_collapses_to_one_finding_id() {
    // Simulates codegen flood: the same (run, task) gate is re-checked multiple
    // times. Each invocation appends a line to the findings stream, but they all
    // share the SAME finding-id, so the review-queue (which dedups by
    // finding_id, keeping the newest) collapses them to exactly one row.
    let fx = Fixture::new("escalate-idempotent");
    let rid = fx.init_run(safe_decomp());

    for _ in 0..3 {
        let out = fx.condukt(&["gate", "check", "--run", &rid, "--task", "t1"], false);
        assert_eq!(out.status.code(), Some(1));
    }

    let findings = fx.review_findings();
    assert_eq!(
        findings.len(),
        3,
        "each invocation appends its own line to the raw stream: {findings:?}"
    );

    // Apply the SAME dedup contract `review-queue` uses (dedup by finding_id,
    // keep latest ts) directly over the raw stream, without reaching into
    // overwatch's private review_queue module.
    let mut by_id: std::collections::BTreeMap<String, &overwatch::review_finding::ReviewFinding> =
        std::collections::BTreeMap::new();
    for f in &findings {
        by_id
            .entry(f.finding_id.clone())
            .and_modify(|existing| {
                if f.ts >= existing.ts {
                    *existing = f;
                }
            })
            .or_insert(f);
    }
    assert_eq!(
        by_id.len(),
        1,
        "re-checking the same gate must collapse to ONE ai-finding row, not one per check: {by_id:?}"
    );
    assert!(by_id.contains_key(&format!("gate-exec:{rid}:t1")));
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
