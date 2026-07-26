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

// ── `verify checks` oracle: blastguard `Ask` must harden to `Deny` ───────────
//
// `verify::run_check` (the checks[] oracle spawned by `condukt verify checks`)
// gates every declared command through `blastguard::detect::detect` before
// handing it to `sh -c`. The oracle runs fully non-interactively (no human is
// ever present to answer a blastguard `Ask`), so an `Ask` verdict must be
// hardened to `Deny` and refused fail-closed — matching `Decision::hardened`'s
// own contract (`crates/blastguard/src/model.rs`). Before the fix, only
// `Decision::Deny` was matched, so an `Ask`-classified command string slipped
// through untouched and reached `sh -c`.

/// Write `checks` (a `{"checks":[...]}` document) to a temp file and run
/// `condukt verify checks --file <path>`, returning the parsed report JSON.
fn run_verify_checks(tag: &str, checks_json: &str) -> (std::process::Output, serde_json::Value) {
    let pid = std::process::id();
    let mut dir = std::env::temp_dir();
    dir.push(format!("condukt-verify-checks-cli-{pid}-{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("checks.json");
    std::fs::write(&file, checks_json).unwrap();

    let out = Command::new(bin())
        .args(["verify", "checks", "--file", file.to_str().unwrap()])
        .current_dir(&dir)
        .output()
        .expect("spawn condukt verify checks");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let value: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("parse report JSON: {e}; {out:?}"));
    (out, value)
}

#[test]
fn pin_the_ask_classified_command_used_below() {
    // RED-pin: confirm this exact command string is genuinely classified
    // `Decision::Ask` by blastguard's detector — an unrecognised wrapper head
    // (`my-cleanup-wrapper`) in front of a destructive `rm -rf` tail, the
    // documented "ASK-2: unknown wrapper" case (blastguard detect.rs
    // `unknown_wrapper_ask_covers_every_unrecognized_head`). If blastguard ever
    // reclassifies this string, this assertion (not the refusal test below)
    // is what breaks, keeping the two failure modes distinguishable.
    let cmd = "my-cleanup-wrapper rm -rf /some/path";
    let input = serde_json::json!({ "command": cmd });
    let decision = blastguard::detect::detect("Bash", Some(&input));
    assert!(
        decision.is_ask(),
        "expected Decision::Ask for {cmd:?}, got {decision:?} — pick a different pinned input"
    );
}

#[test]
fn ask_classified_check_command_is_refused_fail_closed_never_spawned() {
    // GREEN (post-fix) behavior: the checks[] oracle hardens Ask -> Deny before
    // spawning, so this never reaches `sh -c`. A refusal is observable as
    // passed:false with exit:-1 (the same fail-soft shape used for a
    // blastguard-Deny and for a genuine spawn failure) — critically NOT the
    // exit code `sh` would produce if it actually tried to run
    // `my-cleanup-wrapper` (a nonexistent binary -> exit 127). exit:-1 here is
    // proof the shell was never invoked, not proof the command failed inside
    // the shell.
    let checks = r#"{"checks":[{"cmd":"my-cleanup-wrapper rm -rf /some/path"}]}"#;
    let (out, report) = run_verify_checks("ask-refused", checks);
    assert!(
        out.status.success(),
        "verify checks itself must exit 0 (the refusal is IN the report, not a CLI failure); out={out:?}"
    );
    assert_eq!(report["verdict"], "failed", "report={report}");
    assert_eq!(report["all_passed"], false, "report={report}");
    let results = report["results"].as_array().expect("results array");
    assert_eq!(results.len(), 1, "report={report}");
    assert_eq!(
        results[0]["passed"], false,
        "an Ask-classified command must be refused, not executed: {report}"
    );
    assert_eq!(
        results[0]["exit"], -1,
        "refusal must short-circuit before spawn (exit -1), never sh's own exit code: {report}"
    );
}

#[test]
fn allow_classified_check_command_still_runs_normally_anti_vacuity() {
    // Anti-vacuity control: hardening Ask->Deny must not over-block ordinary,
    // clearly-safe checks. `true` (POSIX no-op, always exits 0) is
    // Decision::Allow — pin that first, then prove it still actually runs
    // (passed:true, exit:0) through the SAME oracle path exercised above.
    let cmd = "true";
    let input = serde_json::json!({ "command": cmd });
    assert_eq!(
        blastguard::detect::detect("Bash", Some(&input)),
        blastguard::model::Decision::Allow,
        "expected {cmd:?} to be Decision::Allow"
    );

    let checks = r#"{"checks":[{"cmd":"true","expect_exit":0}]}"#;
    let (out, report) = run_verify_checks("allow-runs", checks);
    assert!(out.status.success(), "out={out:?}");
    assert_eq!(report["verdict"], "passed", "report={report}");
    assert_eq!(report["all_passed"], true, "report={report}");
    let results = report["results"].as_array().expect("results array");
    assert_eq!(results.len(), 1, "report={report}");
    assert_eq!(
        results[0]["passed"], true,
        "an Allow-classified benign command must actually run and pass: {report}"
    );
    assert_eq!(results[0]["exit"], 0, "report={report}");
}

#[test]
fn deny_classified_check_command_stays_refused_regression() {
    // Regression: a clearly-destructive Deny input must remain refused exactly
    // as before this change (Deny was already matched pre-fix; this guards
    // against the .hardened() refactor accidentally changing that path).
    let cmd = "rm -rf /";
    let input = serde_json::json!({ "command": cmd });
    assert!(
        blastguard::detect::detect("Bash", Some(&input)).is_deny(),
        "expected {cmd:?} to be Decision::Deny"
    );

    let checks = format!(r#"{{"checks":[{{"cmd":{cmd:?}}}]}}"#);
    let (out, report) = run_verify_checks("deny-refused", &checks);
    assert!(out.status.success(), "out={out:?}");
    assert_eq!(report["verdict"], "failed", "report={report}");
    let results = report["results"].as_array().expect("results array");
    assert_eq!(results[0]["passed"], false, "report={report}");
    assert_eq!(results[0]["exit"], -1, "report={report}");
}
