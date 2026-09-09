// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! End-to-end tests driving the built binary against a throwaway git repo with
//! a *fake* agent (a `bash -c` script), so no real LLM is required. Exercises
//! scope resolution, prompt delivery, marker parsing, and report/sentinel I/O.

use std::fs;
use std::path::Path;
use std::process::Command;

fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .status()
        .expect("git runs");
    assert!(status.success(), "git {args:?} failed");
}

fn init_repo(repo: &Path) -> String {
    git(repo, &["init", "-q"]);
    git(repo, &["config", "user.email", "t@t.t"]);
    git(repo, &["config", "user.name", "t"]);
    git(repo, &["config", "commit.gpgsign", "false"]);
    fs::write(repo.join("README.md"), "seed\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "seed"]);
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Write a config whose "agent" is a bash script that drains stdin and prints
/// `body` verbatim.
fn write_config(repo: &Path, agent_output: &str) {
    // Embed the canned output via a heredoc-free printf-safe single-quoted arg.
    let script = format!("cat >/dev/null; cat <<'SPECGUARD_EOF'\n{agent_output}\nSPECGUARD_EOF");
    let cfg = format!(
        r#"
[project]
name = "Demo"
root = "."

[agent]
command = "bash"
args = ["-c", {script:?}]

[output]
report_dir = "reports"
sentinel = ".pending"

[[area]]
name = "src"
globs = ["src/**"]
canon = ["docs/spec.md"]
"#,
    );
    fs::write(repo.join("specguard.toml"), cfg).unwrap();
}

fn run_specguard(repo: &Path, baseline: &str, sub: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_specguard"))
        .current_dir(repo)
        .args([
            "--config",
            "specguard.toml",
            "--baseline",
            baseline,
            "--date",
            "2026-01-01",
        ])
        .args(sub)
        .output()
        .expect("specguard runs")
}

/// Like [`run_specguard`] but with no `--baseline` override, so the CLI's own
/// resolution precedence (override > `baseline_ref` > recorded ref >
/// `fallback_ref`) runs for real — needed to exercise `map sync`'s baseline
/// wiring, which `run_specguard`'s always-explicit `--baseline` bypasses.
fn run_specguard_no_baseline(repo: &Path, sub: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_specguard"))
        .current_dir(repo)
        .args(["--config", "specguard.toml", "--date", "2026-01-01"])
        .args(sub)
        .output()
        .expect("specguard runs")
}

/// The `last_ref` recorded for `key`'s entry in the map file, if any.
fn entry_last_ref(map_toml: &str, key: &str) -> Option<String> {
    let needle = format!("[entries.\"{key}\"]");
    let start = map_toml.find(&needle)?;
    let block_end = map_toml[start..]
        .find("\n[entries.")
        .map(|i| start + i)
        .unwrap_or(map_toml.len());
    map_toml[start..block_end].lines().find_map(|l| {
        l.strip_prefix("last_ref = \"")
            .and_then(|rest| rest.strip_suffix('"'))
            .map(|s| s.to_string())
    })
}

#[test]
fn run_with_findings_writes_report_and_sentinel() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let base = init_repo(repo);

    // A change inside the "src" area so it lands in scope.
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(repo.join("src/main.rs").as_path(), "fn main() {}\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "add src"]);

    write_config(
        repo,
        "# Demo audit\n\nfinding body\n\n<<<SPEC_AUDIT>>>\nneeds_user: yes\nsummary: fix the drift",
    );

    let out = run_specguard(repo, &base, &["run"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let report = fs::read_to_string(repo.join("reports/2026-01-01.md")).unwrap();
    assert!(report.contains("Demo audit"));
    assert!(report.contains("finding body"));
    // Provenance: the merged report pins the canon commit it judged against.
    assert!(report.contains("canon commit (HEAD):"), "report:\n{report}");
    // Trailer must be stripped from the saved report.
    assert!(!report.contains("<<<SPEC_AUDIT>>>"));

    let sentinel = fs::read_to_string(repo.join(".pending")).unwrap();
    assert!(sentinel.contains("summary: fix the drift"));
    assert!(sentinel.contains("report: reports/2026-01-01.md"));

    // Findings hold the baseline (not advanced) until `ack`, so the same drift is
    // re-detected on the next run.
    assert!(
        !repo.join("reports/.last-ref").exists(),
        "baseline must be held while a sentinel is pending"
    );
}

#[test]
fn run_without_findings_writes_no_sentinel() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let base = init_repo(repo);
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(repo.join("src/x.rs"), "//\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "x"]);

    write_config(
        repo,
        "# clean\n\n<<<SPEC_AUDIT>>>\nneeds_user: no\nsummary: なし",
    );
    let out = run_specguard(repo, &base, &["run"]);
    assert!(out.status.success());
    assert!(repo.join("reports/2026-01-01.md").exists());
    assert!(
        !repo.join(".pending").exists(),
        "no sentinel when no findings"
    );
    // A fully clean run advances the baseline.
    assert!(
        repo.join("reports/.last-ref").exists(),
        "clean run advances baseline"
    );
}

#[test]
fn missing_marker_exits_3_and_no_sentinel() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let base = init_repo(repo);
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(repo.join("src/x.rs"), "//\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "x"]);

    write_config(repo, "# report with no trailer at all");
    let out = run_specguard(repo, &base, &["run"]);
    assert_eq!(out.status.code(), Some(3));
    assert!(!repo.join(".pending").exists());
    // Raw report still saved for inspection.
    assert!(repo.join("reports/2026-01-01.md").exists());
}

#[test]
fn agent_nonzero_exit_maps_to_4_with_true_code_on_stderr() {
    // An agent failure maps to specguard's reserved EXIT_AGENT_FAILED (4), never
    // the raw code (which could collide with 2=usage / 3=no-marker). The real
    // code is surfaced on stderr. No report or sentinel is written.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let base = init_repo(repo);
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(repo.join("src/x.rs"), "//\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "x"]);

    let cfg = r#"
[project]
name = "Demo"
root = "."
[agent]
command = "bash"
args = ["-c", "cat >/dev/null; exit 3"]
[output]
report_dir = "reports"
sentinel = ".pending"
[[area]]
name = "src"
globs = ["src/**"]
"#;
    fs::write(repo.join("specguard.toml"), cfg).unwrap();

    let out = run_specguard(repo, &base, &["run"]);
    assert_eq!(
        out.status.code(),
        Some(4),
        "agent failure -> EXIT_AGENT_FAILED"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("code 3"),
        "true agent code on stderr: {stderr}"
    );
    assert!(!repo.join(".pending").exists());
    assert!(
        !repo.join("reports/2026-01-01.md").exists(),
        "no report on agent failure"
    );
}

#[test]
fn ack_clears_the_sentinel() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let base = init_repo(repo);
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(repo.join("src/main.rs").as_path(), "fn main() {}\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "add src"]);

    write_config(
        repo,
        "# Demo audit\n\nfinding body\n\n<<<SPEC_AUDIT>>>\nneeds_user: yes\nsummary: fix the drift",
    );
    let out = run_specguard(repo, &base, &["run"]);
    assert!(out.status.success());
    assert!(repo.join(".pending").exists(), "sentinel raised");

    // Make a fix commit so the ack guard passes.
    fs::write(
        repo.join("src/main.rs").as_path(),
        "fn main() { /* fixed */ }\n",
    )
    .unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "fix drift"]);

    let out = run_specguard(repo, &base, &["ack"]);
    assert!(out.status.success());
    assert!(!repo.join(".pending").exists(), "ack removed the sentinel");
}

#[test]
fn ack_rejected_when_no_fix_commit() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let base = init_repo(repo);
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(repo.join("src/main.rs").as_path(), "fn main() {}\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "add src"]);

    write_config(
        repo,
        "# audit\n\n<<<SPEC_AUDIT>>>\nneeds_user: yes\nsummary: drift",
    );
    let out = run_specguard(repo, &base, &["run"]);
    assert!(out.status.success());
    assert!(repo.join(".pending").exists(), "sentinel raised");

    // ack without a new commit should be rejected
    let out = run_specguard(repo, &base, &["ack"]);
    assert!(
        !out.status.success(),
        "ack should be rejected without fix commit"
    );
    assert!(repo.join(".pending").exists(), "sentinel still present");
}

#[test]
fn ack_force_bypasses_commit_guard() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let base = init_repo(repo);
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(repo.join("src/main.rs").as_path(), "fn main() {}\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "add src"]);

    write_config(
        repo,
        "# audit\n\n<<<SPEC_AUDIT>>>\nneeds_user: yes\nsummary: drift",
    );
    let out = run_specguard(repo, &base, &["run"]);
    assert!(out.status.success());
    assert!(repo.join(".pending").exists(), "sentinel raised");

    // --force bypasses the guard
    let out = run_specguard(repo, &base, &["ack", "--force"]);
    assert!(
        out.status.success(),
        "ack --force should clear without commit"
    );
    assert!(
        !repo.join(".pending").exists(),
        "sentinel cleared by --force"
    );
}

#[test]
fn pending_sentinel_holds_baseline_until_ack() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let base = init_repo(repo);
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(repo.join("src/main.rs").as_path(), "fn main() {}\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "add src"]);

    // 1. Findings run: sentinel raised, baseline held.
    write_config(
        repo,
        "# audit\n\n<<<SPEC_AUDIT>>>\nneeds_user: yes\nsummary: drift",
    );
    assert!(run_specguard(repo, &base, &["run"]).status.success());
    assert!(repo.join(".pending").exists());
    assert!(!repo.join("reports/.last-ref").exists(), "held on findings");

    // 2. Clean run while the sentinel is still pending: baseline stays held,
    //    sentinel left untouched.
    write_config(
        repo,
        "# clean\n\n<<<SPEC_AUDIT>>>\nneeds_user: no\nsummary: なし",
    );
    assert!(run_specguard(repo, &base, &["run"]).status.success());
    assert!(
        repo.join(".pending").exists(),
        "sentinel untouched while pending"
    );
    assert!(
        !repo.join("reports/.last-ref").exists(),
        "still held pre-ack"
    );

    // 3. After ack, a clean run advances the baseline.
    // Add a fix commit so the ack guard passes.
    fs::write(
        repo.join("src/main.rs").as_path(),
        "fn main() { /* fixed */ }\n",
    )
    .unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "fix drift"]);
    assert!(run_specguard(repo, &base, &["ack"]).status.success());
    assert!(run_specguard(repo, &base, &["run"]).status.success());
    assert!(
        repo.join("reports/.last-ref").exists(),
        "advanced after ack + clean"
    );
}

#[test]
fn scope_subcommand_lists_in_scope_area() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let base = init_repo(repo);
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(repo.join("src/x.rs"), "//\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "x"]);

    write_config(repo, "unused");
    let out = run_specguard(repo, &base, &["scope"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("in-scope areas:"));
    assert!(stdout.contains("- src (1 file(s))"));
}

#[test]
fn unresolvable_baseline_falls_back_to_all_tracked() {
    // Young repo (1 commit) with a bogus baseline and a non-existent fallback:
    // specguard should audit all tracked files instead of hard-erroring.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    init_repo(repo);
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(repo.join("src/x.rs"), "//\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "x"]);

    write_config(repo, "unused");
    let out = run_specguard(repo, "does-not-exist-ref", &["scope"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("- src"),
        "src area should be in scope via all-tracked fallback:\n{stdout}"
    );
}

#[test]
fn prompt_subcommand_renders_without_agent() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let base = init_repo(repo);
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(repo.join("src/x.rs"), "//\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "x"]);

    write_config(repo, "unused");
    let out = run_specguard(repo, &base, &["prompt"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Demo"));
    assert!(stdout.contains("docs/spec.md"));
    assert!(stdout.contains("<<<SPEC_AUDIT>>>"));
    assert!(!stdout.contains("{{"));
}

// --- Parallel fan-out: two areas + an invariant => three shards audited in
// separate processes and merged. ---

/// Commit a file in each of two areas (`alpha`, `beta`) so both land in scope.
fn commit_two_areas(repo: &Path) {
    fs::create_dir_all(repo.join("alpha")).unwrap();
    fs::create_dir_all(repo.join("beta")).unwrap();
    fs::write(repo.join("alpha/a.rs"), "//\n").unwrap();
    fs::write(repo.join("beta/b.rs"), "//\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "two areas"]);
}

/// Config with two areas + one invariant (=> 3 shards) and a CONTENT-SENSITIVE
/// fake agent: `script` (bash -c) inspects the per-shard prompt on stdin, so we
/// can give each shard a distinct verdict and prove each got its own prompt.
fn write_fanout_config(repo: &Path, script: &str) {
    let cfg = format!(
        r#"
[project]
name = "Demo"
root = "."
[agent]
command = "bash"
args = ["-c", {script:?}]
[output]
report_dir = "reports"
sentinel = ".pending"
[[area]]
name = "alpha"
globs = ["alpha/**"]
[[area]]
name = "beta"
globs = ["beta/**"]
[[invariant]]
name = "inv1"
description = "some rule"
"#,
    );
    fs::write(repo.join("specguard.toml"), cfg).unwrap();
}

#[test]
fn fanout_merges_shards_and_ors_needs_user() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let base = init_repo(repo);
    commit_two_areas(repo);

    // Only the `beta` shard flags needs_user; alpha and the invariant shard are
    // clean. The agent routes on the shard-scope line of each prompt.
    let script = r#"input=$(cat)
if printf '%s' "$input" | grep -q '領域「alpha」'; then
  printf '# alpha audit\n\n<<<SPEC_AUDIT>>>\nneeds_user: no\nsummary: なし\n'
elif printf '%s' "$input" | grep -q '領域「beta」'; then
  printf '# beta audit\n\n<<<SPEC_AUDIT>>>\nneeds_user: yes\nsummary: beta drift\n'
else
  printf '# inv audit\n\n<<<SPEC_AUDIT>>>\nneeds_user: no\nsummary: なし\n'
fi"#;
    write_fanout_config(repo, script);

    let out = run_specguard(repo, &base, &["run"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let report = fs::read_to_string(repo.join("reports/2026-01-01.md")).unwrap();
    // All three shards are present and merged (each got its own focused prompt).
    assert!(report.contains("## shard: alpha"), "report:\n{report}");
    assert!(report.contains("## shard: beta"), "report:\n{report}");
    assert!(report.contains("## shard: invariants"), "report:\n{report}");
    assert!(report.contains("alpha audit") && report.contains("beta audit"));
    // Every shard's trailer is stripped from the merged report.
    assert!(!report.contains("<<<SPEC_AUDIT>>>"));

    // needs_user is OR'd across shards: beta alone flagged -> sentinel raised.
    // Exactly one flagged shard -> summary is verbatim (no label prefix).
    let sentinel = fs::read_to_string(repo.join(".pending")).unwrap();
    assert!(
        sentinel.contains("summary: beta drift"),
        "sentinel:\n{sentinel}"
    );
    // Findings pending -> baseline held.
    assert!(!repo.join("reports/.last-ref").exists());
}

#[test]
fn fanout_labels_summary_when_multiple_shards_flag() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let base = init_repo(repo);
    commit_two_areas(repo);

    // Both area shards flag -> the merged summary labels each contribution.
    let script = r#"input=$(cat)
if printf '%s' "$input" | grep -q '領域「alpha」'; then
  printf '# alpha audit\n\n<<<SPEC_AUDIT>>>\nneeds_user: yes\nsummary: alpha drift\n'
elif printf '%s' "$input" | grep -q '領域「beta」'; then
  printf '# beta audit\n\n<<<SPEC_AUDIT>>>\nneeds_user: yes\nsummary: beta drift\n'
else
  printf '# inv audit\n\n<<<SPEC_AUDIT>>>\nneeds_user: no\nsummary: なし\n'
fi"#;
    write_fanout_config(repo, script);

    let out = run_specguard(repo, &base, &["run"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let sentinel = fs::read_to_string(repo.join(".pending")).unwrap();
    assert!(
        sentinel.contains("[alpha] alpha drift"),
        "sentinel:\n{sentinel}"
    );
    assert!(
        sentinel.contains("[beta] beta drift"),
        "sentinel:\n{sentinel}"
    );
}

// --- `specguard pending`: SessionStart hook entry point (fix-offer). ---

#[test]
fn pending_is_silent_without_sentinel_and_offers_fix_with_one() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    init_repo(repo);
    write_config(repo, "unused"); // sets sentinel = ".pending"

    // No sentinel -> nothing printed (never blocks the session).
    let none = run_specguard(repo, "HEAD", &["pending"]);
    assert!(none.status.success());
    assert!(none.stdout.is_empty(), "silent when no sentinel");

    // Sentinel at the CONFIGURED path (not a hardcoded one) -> fix-offer block.
    fs::write(
        repo.join(".pending"),
        "date: 2026-01-01\nreport: reports/2026-01-01.md\nsummary: fix the drift\n",
    )
    .unwrap();
    let out = run_specguard(repo, "HEAD", &["pending"]);
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("未処理の仕様ドリフト"), "surfaces drift: {s}");
    assert!(
        s.contains("reports/2026-01-01.md"),
        "includes the report path"
    );
    assert!(s.contains("fix the drift"), "includes the summary");
    assert!(s.contains("AskUserQuestion"), "drives the active fix-offer");
}

// --- `specguard brief`: read-only pre-task spec briefing. ---

#[test]
fn brief_renders_prompt_and_runs_agent() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    init_repo(repo);
    write_config(repo, "## spec-brief: do X\n\nbriefing body"); // fake agent echoes this

    // --prompt: render only (no agent). Task is embedded, every area's canon is
    // listed, and no placeholder leaks.
    let p = run_specguard(repo, "HEAD", &["brief", "Add a new endpoint", "--prompt"]);
    assert!(
        p.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&p.stderr)
    );
    let ps = String::from_utf8_lossy(&p.stdout);
    assert!(ps.contains("Add a new endpoint"), "task embedded: {ps}");
    assert!(ps.contains("docs/spec.md"), "area canon listed");
    assert!(!ps.contains("{{"), "no leftover placeholders");

    // default: runs the (fake) agent and prints its brief verbatim.
    let r = run_specguard(repo, "HEAD", &["brief", "Add a new endpoint"]);
    assert!(
        r.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&r.stderr)
    );
    assert!(String::from_utf8_lossy(&r.stdout).contains("spec-brief: do X"));
}

// --- `specguard init`: scaffold config + Claude Code SessionStart hook. ---

fn specguard_init(repo: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_specguard"))
        .current_dir(repo)
        .args(["--config", "specguard.toml", "init"])
        .args(args)
        .output()
        .expect("init runs")
}

#[test]
fn init_scaffolds_config_and_hook_idempotently() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();

    let out = specguard_init(repo, &[]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let cfg = fs::read_to_string(repo.join("specguard.toml")).unwrap();
    assert!(cfg.contains("[[area]]"), "starter config scaffolded");

    let settings = fs::read_to_string(repo.join(".claude/settings.json")).unwrap();
    assert!(settings.contains("SessionStart"));
    assert!(
        settings.contains("specguard pending"),
        "hook delegates to the binary"
    );
    assert_eq!(settings.matches("\"matcher\"").count(), 1, "one hook group");

    // Re-running init must not duplicate the hook nor clobber the config.
    fs::write(repo.join("specguard.toml"), "name = \"edited\"\n").unwrap();
    let out2 = specguard_init(repo, &[]);
    assert!(out2.status.success());
    let settings2 = fs::read_to_string(repo.join(".claude/settings.json")).unwrap();
    assert_eq!(
        settings2.matches("\"matcher\"").count(),
        1,
        "hook not duplicated"
    );
    assert_eq!(
        fs::read_to_string(repo.join("specguard.toml")).unwrap(),
        "name = \"edited\"\n",
        "existing config not clobbered without --force"
    );

    // --force overwrites the config back to the example.
    assert!(specguard_init(repo, &["--force"]).status.success());
    assert!(fs::read_to_string(repo.join("specguard.toml"))
        .unwrap()
        .contains("[[area]]"));
}

#[test]
fn init_preserves_existing_settings() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    fs::create_dir_all(repo.join(".claude")).unwrap();
    fs::write(
        repo.join(".claude/settings.json"),
        r#"{"model":"opus","hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[]}]}}"#,
    )
    .unwrap();

    let out = specguard_init(repo, &[]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let s = fs::read_to_string(repo.join(".claude/settings.json")).unwrap();
    assert!(s.contains("\"model\""), "unrelated keys preserved");
    assert!(s.contains("PreToolUse"), "existing hooks preserved");
    assert!(s.contains("SessionStart"), "our hook added");
    assert!(s.contains("specguard pending"));
}

// --- Decision records (ADR) + canon-change trigger + D3 audit. ---

#[test]
fn decide_scaffolds_pinned_record_idempotently() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let base = init_repo(repo);
    write_config(repo, "unused");

    let out = run_specguard(repo, &base, &["decide", "Single signing path"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let rec = repo.join("decisions/2026-01-01-single-signing-path.md");
    let body = fs::read_to_string(&rec).expect("record written");
    assert!(body.contains("canon_commit: "), "pinned to a canon commit");
    assert!(body.contains("status: proposed"));
    assert!(body.contains("Single signing path"));
    assert!(body.contains("drivers: []"));

    // Re-running without --force must not overwrite (errors as usage = exit 2).
    fs::write(&rec, "edited\n").unwrap();
    let dup = run_specguard(repo, &base, &["decide", "Single signing path"]);
    assert_eq!(dup.status.code(), Some(2), "duplicate id rejected");
    assert_eq!(
        fs::read_to_string(&rec).unwrap(),
        "edited\n",
        "not overwritten"
    );

    // --force overwrites.
    let forced = run_specguard(repo, &base, &["decide", "Single signing path", "--force"]);
    assert!(forced.status.success());
    assert!(fs::read_to_string(&rec)
        .unwrap()
        .contains("status: proposed"));
}

#[test]
fn canon_change_triggers_area_without_code_change() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let base = init_repo(repo);
    // Change only the area's canon doc (docs/spec.md is `src`'s canon), no code.
    fs::create_dir_all(repo.join("docs")).unwrap();
    fs::write(repo.join("docs/spec.md"), "rule v2\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "spec change only"]);

    write_config(repo, "unused");
    let out = run_specguard(repo, &base, &["scope"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("- src (0 file(s), canon changed: 1)"),
        "area in scope via canon change:\n{stdout}"
    );
}

#[test]
fn d3_decisions_shard_runs_alongside_areas() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let base = init_repo(repo);
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(repo.join("src/x.rs"), "//\n").unwrap();
    // A decision record present -> D3 shard activates.
    fs::create_dir_all(repo.join("decisions")).unwrap();
    fs::write(repo.join("decisions/2026-01-01-x.md"), "---\nid: x\n---\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "code + decision"]);

    // Content-sensitive agent: route the D3 shard vs the area/invariant shards.
    let script = r#"input=$(cat)
if printf '%s' "$input" | grep -q '(D3)'; then
  printf '# D3 audit\n\n<<<SPEC_AUDIT>>>\nneeds_user: no\nsummary: なし\n'
else
  printf '# area audit\n\n<<<SPEC_AUDIT>>>\nneeds_user: no\nsummary: なし\n'
fi"#;
    let cfg = format!(
        r#"
[project]
name = "Demo"
root = "."
[agent]
command = "bash"
args = ["-c", {script:?}]
[output]
report_dir = "reports"
sentinel = ".pending"
[[area]]
name = "src"
globs = ["src/**"]
canon = ["docs/spec.md"]
"#,
    );
    fs::write(repo.join("specguard.toml"), cfg).unwrap();

    let out = run_specguard(repo, &base, &["run"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report = fs::read_to_string(repo.join("reports/2026-01-01.md")).unwrap();
    assert!(report.contains("## shard: src"), "report:\n{report}");
    assert!(report.contains("## shard: decisions"), "report:\n{report}");
    assert!(report.contains("D3 audit"), "D3 body merged:\n{report}");
}

// --- Verification gates: V1 refute (false positives) + V2 completeness
//     (false negatives). See DESIGN-VERIFY.md. ---

/// Config with a single `src` area, a content-sensitive fake agent (routes on the
/// prompt kind: audit / refute / completeness), and a `[verify]` table.
fn write_verify_config(repo: &Path, script: &str, verify_toml: &str) {
    let cfg = format!(
        r#"
[project]
name = "Demo"
root = "."
[agent]
command = "bash"
args = ["-c", {script:?}]
[output]
report_dir = "reports"
sentinel = ".pending"
{verify_toml}
[[area]]
name = "src"
globs = ["src/**"]
canon = ["docs/spec.md"]
"#,
    );
    fs::write(repo.join("specguard.toml"), cfg).unwrap();
}

fn seed_src_area(repo: &Path) {
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(repo.join("src/main.rs").as_path(), "fn main() {}\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "add src"]);
}

#[test]
fn refute_drops_false_positive_and_clears_sentinel() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let base = init_repo(repo);
    seed_src_area(repo);

    // Audit flags a finding; the skeptic refutes it -> post-verify clean.
    let script = r#"input=$(cat)
if printf '%s' "$input" | grep -q '反証監査'; then
  printf '## 反証結果\nDROP: 引用が verdict を支持せず\n\n<<<SPEC_AUDIT>>>\nneeds_user: no\nsummary: なし\n'
else
  printf '# audit\n\nD1 finding X\n\n<<<SPEC_AUDIT>>>\nneeds_user: yes\nsummary: drift X\n'
fi"#;
    write_verify_config(repo, script, "[verify]\nenabled = true");

    let out = run_specguard(repo, &base, &["run"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let report = fs::read_to_string(repo.join("reports/2026-01-01.md")).unwrap();
    // Transparency: both the original finding and the refutation are present.
    assert!(
        report.contains("D1 finding X"),
        "original finding kept in report:\n{report}"
    );
    assert!(
        report.contains("反証 (verify)"),
        "refutation section present"
    );
    assert!(report.contains("引用が verdict を支持せず"));
    // Refuted away -> no sentinel, and a fully clean run advances the baseline.
    assert!(
        !repo.join(".pending").exists(),
        "false positive refuted -> no sentinel"
    );
    assert!(
        repo.join("reports/.last-ref").exists(),
        "clean post-verify advances baseline"
    );
}

#[test]
fn refute_keeps_upheld_finding_and_raises_sentinel() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let base = init_repo(repo);
    seed_src_area(repo);

    // The skeptic upholds the finding -> it survives to the human.
    let script = r#"input=$(cat)
if printf '%s' "$input" | grep -q '反証監査'; then
  printf '## 反証結果\nKEEP: 逐語引用で覆せない\n\n<<<SPEC_AUDIT>>>\nneeds_user: yes\nsummary: drift X 確定\n'
else
  printf '# audit\n\nD1 finding X\n\n<<<SPEC_AUDIT>>>\nneeds_user: yes\nsummary: drift X\n'
fi"#;
    write_verify_config(repo, script, "[verify]\nenabled = true");

    let out = run_specguard(repo, &base, &["run"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let sentinel = fs::read_to_string(repo.join(".pending")).unwrap();
    // Post-verify summary takes over from the audit's.
    assert!(
        sentinel.contains("summary: drift X 確定"),
        "sentinel:\n{sentinel}"
    );
    assert!(
        !repo.join("reports/.last-ref").exists(),
        "findings hold the baseline"
    );
}

#[test]
fn inconclusive_refute_keeps_findings_failsafe() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let base = init_repo(repo);
    seed_src_area(repo);

    // The skeptic fails (nonzero exit, no marker) on the refute pass only; the
    // audit pass succeeds. A broken verifier must NOT drop the finding.
    let script = r#"input=$(cat)
if printf '%s' "$input" | grep -q '反証監査'; then
  printf 'skeptic crashed\n' >&2; exit 1
else
  printf '# audit\n\nD1 finding X\n\n<<<SPEC_AUDIT>>>\nneeds_user: yes\nsummary: drift X\n'
fi"#;
    write_verify_config(repo, script, "[verify]\nenabled = true");

    let out = run_specguard(repo, &base, &["run"]);
    // The run still succeeds (verify failure is non-fatal) and keeps the finding.
    assert!(
        out.status.success(),
        "verify failure must not abort the run; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        repo.join(".pending").exists(),
        "fail-safe: finding kept -> sentinel raised"
    );
    let report = fs::read_to_string(repo.join("reports/2026-01-01.md")).unwrap();
    assert!(
        report.contains("反証不能"),
        "inconclusive annotated:\n{report}"
    );
}

#[test]
fn completeness_surfaces_missed_rule_on_clean_audit() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let base = init_repo(repo);
    seed_src_area(repo);

    // Audit is clean, but the completeness critic finds an unmatched canon rule.
    let script = r#"input=$(cat)
if printf '%s' "$input" | grep -q '網羅性批評'; then
  printf '## 網羅性批評\n未照合ルール R5\n\n<<<SPEC_AUDIT>>>\nneeds_user: yes\nsummary: R5 未照合\n'
else
  printf '# audit\n\n<<<SPEC_AUDIT>>>\nneeds_user: no\nsummary: なし\n'
fi"#;
    write_verify_config(repo, script, "[verify]\ncompleteness = true");

    let out = run_specguard(repo, &base, &["run"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let report = fs::read_to_string(repo.join("reports/2026-01-01.md")).unwrap();
    assert!(
        report.contains("## shard: completeness:src"),
        "critic shard merged:\n{report}"
    );
    assert!(report.contains("未照合ルール R5"));
    let sentinel = fs::read_to_string(repo.join(".pending")).unwrap();
    assert!(
        sentinel.contains("R5 未照合"),
        "missed rule raises the sentinel:\n{sentinel}"
    );
    assert!(
        !repo.join("reports/.last-ref").exists(),
        "held while sentinel pending"
    );
}

#[test]
fn verify_off_by_default_is_a_noop() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let base = init_repo(repo);
    seed_src_area(repo);

    // No [verify] table -> neither gate runs. The agent should be asked exactly
    // once (the audit); a refute/completeness prompt would route to a crash.
    let script = r#"input=$(cat)
if printf '%s' "$input" | grep -qE '反証監査|網羅性批評'; then
  printf 'verify ran but should not have\n' >&2; exit 1
else
  printf '# audit\n\nfinding\n\n<<<SPEC_AUDIT>>>\nneeds_user: yes\nsummary: drift\n'
fi"#;
    write_verify_config(repo, script, "");

    let out = run_specguard(repo, &base, &["run"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report = fs::read_to_string(repo.join("reports/2026-01-01.md")).unwrap();
    assert!(
        !report.contains("反証 (verify)"),
        "no verify section when off"
    );
    assert!(repo.join(".pending").exists());
}

// --- Prompt ratification gate (meta-canon acceptance). ---

/// A minimal but contract-valid custom audit template (all required placeholders).
const VALID_TMPL: &str =
    "{{PROJECT_NAME}} {{DATE}} {{SCOPE_SUMMARY}} {{AREAS}} {{INVARIANTS}}\n{{MARKER}}\n";

fn write_ratify_config(repo: &Path, template_body: &str) {
    fs::write(repo.join("tmpl.md"), template_body).unwrap();
    let agent = "# clean\\n\\n<<<SPEC_AUDIT>>>\\nneeds_user: no\\nsummary: なし";
    let script = format!("cat >/dev/null; printf '{agent}\\n'");
    let cfg = format!(
        r#"
[project]
name = "Demo"
root = "."
[agent]
command = "bash"
args = ["-c", {script:?}]
[output]
report_dir = "reports"
sentinel = ".pending"
[prompt]
template = "tmpl.md"
require_ratification = true
[[area]]
name = "src"
globs = ["src/**"]
"#,
    );
    fs::write(repo.join("specguard.toml"), cfg).unwrap();
}

fn seed_src(repo: &Path) {
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(repo.join("src/x.rs"), "//\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "x"]);
}

#[test]
fn run_blocked_until_prompt_ratified_then_passes() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let base = init_repo(repo);
    seed_src(repo);
    write_ratify_config(repo, VALID_TMPL);

    // Unratified prompt -> run is gated (exit 5), no report.
    let blocked = run_specguard(repo, &base, &["run"]);
    assert_eq!(
        blocked.status.code(),
        Some(5),
        "unratified prompt blocks run"
    );
    assert!(!repo.join("reports/2026-01-01.md").exists());

    // Ratify with a rationale -> lock written, pinned to a canon commit.
    let acc = run_specguard(repo, &base, &["accept-prompt", "-m", "initial policy ok"]);
    assert!(
        acc.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&acc.stderr)
    );
    let lock = fs::read_to_string(repo.join(".specguard-prompt.lock")).unwrap();
    assert!(lock.contains("audit_hash ="));
    assert!(lock.contains("reason = \"initial policy ok\""));
    assert!(lock.contains("canon_commit ="));

    // Now the run proceeds.
    let ok = run_specguard(repo, &base, &["run"]);
    assert!(
        ok.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&ok.stderr)
    );
    assert!(repo.join("reports/2026-01-01.md").exists());
}

#[test]
fn changed_prompt_reblocks_until_reratified() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let base = init_repo(repo);
    seed_src(repo);
    write_ratify_config(repo, VALID_TMPL);
    assert!(run_specguard(repo, &base, &["accept-prompt", "-m", "ok"])
        .status
        .success());
    assert!(run_specguard(repo, &base, &["run"]).status.success());

    // Edit the prompt (still contract-valid) -> fingerprint changes -> gated.
    fs::write(
        repo.join("tmpl.md"),
        format!("{VALID_TMPL}\n<!-- policy tweak -->\n"),
    )
    .unwrap();
    let blocked = run_specguard(repo, &base, &["run"]);
    assert_eq!(blocked.status.code(), Some(5), "changed prompt re-blocks");

    // Re-ratify -> passes again.
    assert!(
        run_specguard(repo, &base, &["accept-prompt", "-m", "reviewed tweak"])
            .status
            .success()
    );
    assert!(run_specguard(repo, &base, &["run"]).status.success());
}

#[test]
fn accept_prompt_refuses_contract_violating_template() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let base = init_repo(repo);
    seed_src(repo);
    // Template missing {{MARKER}} -> contradicts the parser contract.
    write_ratify_config(
        repo,
        "{{PROJECT_NAME}} {{DATE}} {{SCOPE_SUMMARY}} {{AREAS}} {{INVARIANTS}}\n",
    );

    let out = run_specguard(repo, &base, &["accept-prompt", "-m", "x"]);
    assert_eq!(out.status.code(), Some(2), "contract violation refused");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("MARKER"),
        "names the missing placeholder: {stderr}"
    );
    assert!(
        !repo.join(".specguard-prompt.lock").exists(),
        "no lock on refusal"
    );
}

/// Ratify config with `[prompt].graded` on at `threshold`, template at
/// `template_body`.
fn write_graded_config(repo: &Path, template_body: &str, threshold: f64) {
    fs::write(repo.join("tmpl.md"), template_body).unwrap();
    let agent = "# clean\\n\\n<<<SPEC_AUDIT>>>\\nneeds_user: no\\nsummary: なし";
    let script = format!("cat >/dev/null; printf '{agent}\\n'");
    let cfg = format!(
        r#"
[project]
name = "Demo"
root = "."
[agent]
command = "bash"
args = ["-c", {script:?}]
[output]
report_dir = "reports"
sentinel = ".pending"
[prompt]
template = "tmpl.md"
require_ratification = true
graded = true
graded_threshold = {threshold}
[[area]]
name = "src"
globs = ["src/**"]
"#,
    );
    fs::write(repo.join("specguard.toml"), cfg).unwrap();
}

#[test]
fn graded_precedented_change_with_contract_violation_is_not_auto_ratified() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let base = init_repo(repo);
    seed_src(repo);

    // Ratify a valid template with graded triage on (low threshold so a small
    // edit is judged precedented).
    write_graded_config(repo, VALID_TMPL, 0.5);
    assert!(
        run_specguard(repo, &base, &["accept-prompt", "-m", "initial"])
            .status
            .success()
    );
    let lock_before = fs::read_to_string(repo.join(".specguard-prompt.lock")).unwrap();
    assert!(run_specguard(repo, &base, &["run"]).status.success());
    // Clear the report from the compliant first run so the later assertion
    // proves the SECOND (blocked) run did not produce a fresh report.
    let _ = fs::remove_file(repo.join("reports/2026-01-01.md"));

    // Drift the template with a SMALL, textually-similar edit that nonetheless
    // drops a required placeholder ({{MARKER}}) — a contract violation. Because
    // the edit is tiny, deterministic similarity to the ratified precedent stays
    // high, so the graded gate's own similarity/polarity triage alone would call
    // this "precedented". The contract check must still refuse auto-ratify.
    let contract_violating = VALID_TMPL.replace("{{MARKER}}\n", "");
    fs::write(repo.join("tmpl.md"), &contract_violating).unwrap();

    let out = run_specguard(repo, &base, &["run"]);
    assert_eq!(
        out.status.code(),
        Some(5),
        "precedented-but-contract-violating change must NOT auto-ratify (stderr: {})",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("MARKER"),
        "names the missing placeholder: {stderr}"
    );

    // The lock must be untouched — write_lock was never called on this path.
    let lock_after = fs::read_to_string(repo.join(".specguard-prompt.lock")).unwrap();
    assert_eq!(
        lock_before, lock_after,
        "ratify lock must not change when the contract check refuses auto-ratify"
    );
    assert!(
        !repo.join("reports/2026-01-01.md").exists(),
        "audit must not run past the unratified gate"
    );
}

#[test]
fn graded_precedented_compliant_change_still_auto_ratifies() {
    // Non-regression: a precedented change that DOES satisfy the contract must
    // keep auto-ratifying exactly as before this fix.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let base = init_repo(repo);
    seed_src(repo);

    write_graded_config(repo, VALID_TMPL, 0.5);
    assert!(
        run_specguard(repo, &base, &["accept-prompt", "-m", "initial"])
            .status
            .success()
    );
    let lock_before = fs::read_to_string(repo.join(".specguard-prompt.lock")).unwrap();
    assert!(run_specguard(repo, &base, &["run"]).status.success());

    // Small, contract-preserving edit (adds a trailing comment) -> still
    // precedented, and now compliant -> should auto-ratify and proceed.
    let compliant_edit = format!("{VALID_TMPL}<!-- tweak -->\n");
    fs::write(repo.join("tmpl.md"), &compliant_edit).unwrap();

    let out = run_specguard(repo, &base, &["run"]);
    assert!(
        out.status.success(),
        "compliant precedented change should still auto-ratify: stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lock_after = fs::read_to_string(repo.join(".specguard-prompt.lock")).unwrap();
    assert_ne!(
        lock_before, lock_after,
        "auto-ratify should re-pin the lock to the new precedent"
    );
    assert!(repo.join("reports/2026-01-01.md").exists());
}

/// Ratify config with an extra `[verify]` table injected verbatim.
fn write_ratify_config_with(repo: &Path, template_body: &str, verify_toml: &str) {
    fs::write(repo.join("tmpl.md"), template_body).unwrap();
    let agent = "# clean\\n\\n<<<SPEC_AUDIT>>>\\nneeds_user: no\\nsummary: なし";
    let script = format!("cat >/dev/null; printf '{agent}\\n'");
    let cfg = format!(
        r#"
[project]
name = "Demo"
root = "."
[agent]
command = "bash"
args = ["-c", {script:?}]
[output]
report_dir = "reports"
sentinel = ".pending"
[prompt]
template = "tmpl.md"
require_ratification = true
{verify_toml}
[[area]]
name = "src"
globs = ["src/**"]
"#,
    );
    fs::write(repo.join("specguard.toml"), cfg).unwrap();
}

#[test]
fn enabling_verify_gate_forces_reratification() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let base = init_repo(repo);
    seed_src(repo);

    // 1. Ratify with verify OFF -> audit-only consent; the refute slot stays empty.
    write_ratify_config_with(repo, VALID_TMPL, "");
    assert!(
        run_specguard(repo, &base, &["accept-prompt", "-m", "audit only"])
            .status
            .success()
    );
    assert!(run_specguard(repo, &base, &["run"]).status.success());
    let lock = fs::read_to_string(repo.join(".specguard-prompt.lock")).unwrap();
    assert!(
        lock.contains("refute_hash = \"\""),
        "refute unpinned while gate off:\n{lock}"
    );

    // 2. Turn the refute gate ON -> the now-active policy was never ratified -> blocked.
    write_ratify_config_with(repo, VALID_TMPL, "[verify]\nenabled = true");
    let blocked = run_specguard(repo, &base, &["run"]);
    assert_eq!(
        blocked.status.code(),
        Some(5),
        "enabling verify re-blocks until ratified"
    );
    let stderr = String::from_utf8_lossy(&blocked.stderr);
    assert!(
        stderr.contains("refute-prompt"),
        "names the unratified verify policy: {stderr}"
    );

    // 3. Re-ratify (now pins the refute policy) -> run passes again.
    let acc = run_specguard(
        repo,
        &base,
        &["accept-prompt", "-m", "reviewed refute policy"],
    );
    assert!(
        acc.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&acc.stderr)
    );
    let lock2 = fs::read_to_string(repo.join(".specguard-prompt.lock")).unwrap();
    assert!(
        !lock2.contains("refute_hash = \"\""),
        "refute now pinned:\n{lock2}"
    );
    assert!(run_specguard(repo, &base, &["run"]).status.success());
}

// --- Subscription-native split: `prompt --json` (harness renders shards) +
// `ingest` (harness parses pre-collected subagent outputs) reproduce `run`
// without the binary ever spawning an agent. ---

/// Run specguard feeding `stdin_data` on stdin (for `ingest`).
fn run_specguard_stdin(
    repo: &Path,
    baseline: &str,
    sub: &[&str],
    stdin_data: &str,
) -> std::process::Output {
    use std::io::Write;
    let mut child = Command::new(env!("CARGO_BIN_EXE_specguard"))
        .current_dir(repo)
        .args([
            "--config",
            "specguard.toml",
            "--baseline",
            baseline,
            "--date",
            "2026-01-01",
        ])
        .args(sub)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("specguard spawns");
    if let Some(mut child_stdin) = child.stdin.take() {
        let _ = child_stdin.write_all(stdin_data.as_bytes());
    }
    child.wait_with_output().expect("specguard runs")
}

#[test]
fn prompt_json_then_ingest_reproduces_run() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let base = init_repo(repo);
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(repo.join("src/main.rs"), "fn main() {}\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "add src"]);

    // The agent here is never invoked by `prompt --json`/`ingest`; it only needs
    // to be a valid config entry.
    write_config(repo, "unused");

    // 1. Harness renders the shard prompts (no agent).
    let pj = run_specguard(repo, &base, &["prompt", "--json"]);
    assert!(
        pj.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&pj.stderr)
    );
    let env: serde_json::Value = serde_json::from_slice(&pj.stdout).expect("valid JSON envelope");
    assert_eq!(env["marker"], "<<<SPEC_AUDIT>>>");
    let shards = env["shards"].as_array().expect("shards array");
    assert_eq!(shards.len(), 1, "one in-scope area => one shard");
    let label = shards[0]["label"].as_str().unwrap().to_string();
    assert!(shards[0]["prompt"]
        .as_str()
        .unwrap()
        .contains("docs/spec.md"));

    // 2. The plugin would dispatch each prompt to a read-only subagent; here we
    // hand-build the outputs and feed them back via `ingest`.
    let ingest_input = serde_json::json!({
        "shards": [
            { "label": label, "stdout": "# audit\n\nbody\n\n<<<SPEC_AUDIT>>>\nneeds_user: yes\nsummary: ingested drift", "code": 0 }
        ]
    })
    .to_string();
    let ing = run_specguard_stdin(repo, &base, &["ingest"], &ingest_input);
    assert!(
        ing.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&ing.stderr)
    );

    // Same outcome as `run`: report written, sentinel raised, baseline held.
    let report = fs::read_to_string(repo.join("reports/2026-01-01.md")).unwrap();
    assert!(report.contains("body"));
    assert!(!report.contains("<<<SPEC_AUDIT>>>"), "trailer stripped");
    let sentinel = fs::read_to_string(repo.join(".pending")).unwrap();
    assert!(
        sentinel.contains("summary: ingested drift"),
        "sentinel:\n{sentinel}"
    );
    assert!(
        !repo.join("reports/.last-ref").exists(),
        "findings hold the baseline"
    );
}

#[test]
fn ingest_missing_shard_output_maps_to_agent_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let base = init_repo(repo);
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(repo.join("src/main.rs"), "fn main() {}\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "add src"]);
    write_config(repo, "unused");

    // A shard the plugin failed to return -> harness treats it as a failed shard.
    let ing = run_specguard_stdin(repo, &base, &["ingest"], r#"{"shards":[]}"#);
    assert_eq!(
        ing.status.code(),
        Some(4),
        "missing shard output => EXIT_AGENT_FAILED"
    );
    let stderr = String::from_utf8_lossy(&ing.stderr);
    assert!(
        stderr.contains("no output provided"),
        "stderr names the gap: {stderr}"
    );
}

#[test]
fn audit_only_project_never_asked_to_ratify_verify() {
    // A project that never enables verify must not be re-blocked by the new
    // policy surface (backward compatibility / no inert-policy gating).
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let base = init_repo(repo);
    seed_src(repo);
    write_ratify_config_with(repo, VALID_TMPL, "");
    assert!(run_specguard(repo, &base, &["accept-prompt", "-m", "ok"])
        .status
        .success());
    // Repeated runs stay green with no verify table present.
    assert!(run_specguard(repo, &base, &["run"]).status.success());
    assert!(run_specguard(repo, &base, &["run"]).status.success());
}

#[test]
fn map_sync_anchors_on_map_last_synced_not_fallback_ref() {
    // Regression: `map sync` must resolve its incremental baseline from the
    // map's own persisted `last_synced` ref, not the unrelated `specguard run`
    // audit `.last-ref` (which never exists for map-only usage and previously
    // made `sync` silently fall back to `fallback_ref`, rescanning a window far
    // wider than "since the map was last synced" and flagging untouched
    // already-tracked files as `changed`).
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    init_repo(repo);
    fs::write(
        repo.join("specguard.toml"),
        r#"
[project]
name = "Demo"
root = "."

[output]
report_dir = "reports"
sentinel = ".pending"

[scope]
fallback_ref = "HEAD~3"

[[area]]
name = "src"
globs = ["src/**"]
canon = ["docs/spec.md"]
"#,
    )
    .unwrap();

    fs::create_dir_all(repo.join("src")).unwrap();
    for name in ["filler_a", "filler_b", "filler_c"] {
        fs::write(repo.join(format!("src/{name}.rs")), "//\n").unwrap();
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-q", "-m", name]);
    }

    // `map build` at this HEAD: fallback_ref=HEAD~3 reaches back to the seed
    // commit, so all three filler files are picked up and stamped with this
    // HEAD as their `last_ref`.
    let built = run_specguard_no_baseline(repo, &["map", "build"]);
    assert!(
        built.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&built.stderr)
    );
    let map_path = repo.join(".specguard/spec-map.toml");
    let after_build = fs::read_to_string(&map_path).unwrap();
    let build_ref =
        entry_last_ref(&after_build, "src/filler_b.rs").expect("filler_b tracked after build");

    // One more commit, then `map sync` with NO --baseline override — the exact
    // path that only has the map's own `last_synced` to anchor on.
    fs::write(repo.join("src/filler_d.rs"), "//\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "filler_d"]);

    let synced = run_specguard_no_baseline(repo, &["map", "sync"]);
    assert!(
        synced.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&synced.stderr)
    );
    let after_sync = fs::read_to_string(&map_path).unwrap();

    // filler_b/filler_c were already synced as of `build_ref` and are untouched
    // by the filler_d commit: a correctly-anchored sync (baseline = build_ref)
    // must not re-touch them, so their `last_ref` stays exactly `build_ref`.
    // With the bug (baseline falling back to HEAD~3 relative to the NEW HEAD),
    // they fall inside the wider rescanned window and get bumped/re-flagged.
    assert_eq!(
        entry_last_ref(&after_sync, "src/filler_b.rs").as_deref(),
        Some(build_ref.as_str()),
        "filler_b must not be re-touched by a sync anchored on the map's own last_synced:\n{after_sync}"
    );
    assert_eq!(
        entry_last_ref(&after_sync, "src/filler_c.rs").as_deref(),
        Some(build_ref.as_str()),
        "filler_c must not be re-touched by a sync anchored on the map's own last_synced:\n{after_sync}"
    );
    // filler_d is genuinely new since `build_ref` and must show up.
    assert!(
        after_sync.contains("[entries.\"src/filler_d.rs\"]"),
        "filler_d (added since build_ref) must be tracked:\n{after_sync}"
    );
}

// --- `specguard brief --json`: the machine-readable tri-state canon-coverage
// verdict (specs/spec-loop.toml R3-brief-emits-tri-state-verdict). ---

/// Write a spec map at the default store path with the given entry tables.
fn write_map(repo: &Path, entries: &[String]) -> std::path::PathBuf {
    let path = repo.join(".specguard/spec-map.toml");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut body = String::from("last_synced = \"deadbeef\"\n");
    for e in entries {
        body.push_str(e);
    }
    fs::write(&path, body).unwrap();
    path
}

/// One `[entries.<key>]` table. `spec_doc = None` means the canon for that
/// entry has not been authored yet.
fn map_entry(key: &str, spec_doc: Option<&str>, impl_files: &[&str]) -> String {
    let mut s =
        format!("[entries.\"{key}\"]\nkey = \"{key}\"\nkind = \"feature\"\nstatus = \"tracked\"\n");
    if let Some(d) = spec_doc {
        s.push_str(&format!("spec_doc = \"{d}\"\n"));
    }
    s.push_str(&format!("impl_files = {impl_files:?}\n"));
    s
}

/// The `verdict` field of a `brief --json` run, plus the raw parsed object.
fn brief_json(repo: &Path, task: &str) -> (std::process::Output, serde_json::Value) {
    let out = run_specguard(repo, "HEAD", &["brief", task, "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "brief --json must print one JSON object; parse failed ({e}).\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    });
    (out, v)
}

fn verdict_of(v: &serde_json::Value) -> String {
    v.get("verdict")
        .and_then(|x| x.as_str())
        .unwrap_or_else(|| panic!("no string `verdict` field in {v}"))
        .to_string()
}

/// The verdict a rendered brief states in its coverage block, read back out of
/// the prose. Exactly one must be stated.
fn prose_verdict(repo: &Path, task: &str) -> String {
    let p = run_specguard(repo, "HEAD", &["brief", task, "--prompt"]);
    assert!(
        p.status.success(),
        "brief --prompt failed: {}",
        String::from_utf8_lossy(&p.stderr)
    );
    let s = String::from_utf8_lossy(&p.stdout).to_string();
    assert!(!s.contains("{{"), "no leftover placeholders: {s}");
    let stated: Vec<String> = ["covered", "not-covered", "undetermined"]
        .into_iter()
        .filter(|t| s.contains(&format!("**{t}**")))
        .map(|t| t.to_string())
        .collect();
    assert_eq!(
        stated.len(),
        1,
        "the brief must state exactly one verdict, found {stated:?} in:\n{s}"
    );
    stated[0].clone()
}

/// R3 acceptance 1: `brief --json` has a `verdict` field, and all THREE of its
/// values are reachable through the real binary.
#[test]
fn brief_json_verdict_field_reaches_all_three_states() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    init_repo(repo);
    write_config(repo, "unused");
    let task = "make coverage.rs emit a tri-state verdict";

    // (1) undetermined — the store was never built here.
    let (out, v) = brief_json(repo, task);
    assert_eq!(verdict_of(&v), "undetermined", "no store: {v}");
    assert_eq!(out.status.code(), Some(10), "undetermined exit code");

    // (2) covered — a matching entry names a spec document.
    write_map(
        repo,
        &[map_entry(
            "specguard-brief",
            Some("docs/DESIGN-spec-loop.md"),
            &["crates/specguard/src/coverage.rs"],
        )],
    );
    let (out, v) = brief_json(repo, task);
    assert_eq!(verdict_of(&v), "covered", "{v}");
    assert_eq!(out.status.code(), Some(0), "an answer exits 0");
    assert_eq!(
        v["entries"],
        serde_json::json!(["specguard-brief"]),
        "covered names the entries it relied on: {v}"
    );

    // (3) not-covered — the store was read and holds no canon for this task.
    write_map(
        repo,
        &[map_entry(
            "specguard-brief",
            None,
            &["crates/specguard/src/coverage.rs"],
        )],
    );
    let (out, v) = brief_json(repo, task);
    assert_eq!(verdict_of(&v), "not-covered", "{v}");
    assert_eq!(out.status.code(), Some(0), "an answer exits 0");
    assert_eq!(
        v["matched_without_spec"],
        serde_json::json!(["specguard-brief"]),
        "not-covered names what it matched: {v}"
    );
}

/// R3 acceptance 2: every state in which the store could not answer resolves to
/// `undetermined`. None of them may reach /flow as `not-covered`, which is the
/// verdict that makes it draft a spec.
#[test]
fn brief_json_unobservable_store_states_are_undetermined_never_not_covered() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    init_repo(repo);
    write_config(repo, "unused");
    let task = "make coverage.rs emit a tri-state verdict";

    let check = |label: &str, out: &std::process::Output, v: &serde_json::Value| {
        assert_eq!(
            verdict_of(v),
            "undetermined",
            "[{label}] must be undetermined, got {v}"
        );
        assert_ne!(
            verdict_of(v),
            "not-covered",
            "[{label}] 'could not observe' must not become 'no spec exists'"
        );
        assert!(
            v.get("reason")
                .and_then(|r| r.as_str())
                .is_some_and(|r| !r.trim().is_empty()),
            "[{label}] undetermined must carry a reason: {v}"
        );
        assert_eq!(
            out.status.code(),
            Some(10),
            "[{label}] undetermined must not exit 0"
        );
    };

    // (a) map file absent.
    let (out, v) = brief_json(repo, task);
    check("absent", &out, &v);

    // (b) map file present but unparseable.
    let path = repo.join(".specguard/spec-map.toml");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "not TOML [[[ entries = ??\n").unwrap();
    let (out, v) = brief_json(repo, task);
    check("corrupt", &out, &v);

    // (c) map file present but unreadable. If this environment cannot make a
    // file unreadable (root, or a mode-ignoring filesystem) the test FAILS
    // rather than silently reporting a path it never observed.
    write_map(
        repo,
        &[map_entry(
            "specguard-brief",
            Some("docs/DESIGN-spec-loop.md"),
            &["crates/specguard/src/coverage.rs"],
        )],
    );
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    assert!(
        fs::read_to_string(&path).is_err(),
        "this environment cannot make a file unreadable, so the permission path \
         is UNOBSERVED — it must not be reported as verified"
    );
    let (out, v) = brief_json(repo, task);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    check("unreadable", &out, &v);

    // (d) map present, zero entries.
    write_map(repo, &[]);
    let (out, v) = brief_json(repo, task);
    check("empty store", &out, &v);

    // (e) populated store, but the task text yields no usable query token, so
    // nothing was ever tested against it.
    write_map(
        repo,
        &[map_entry(
            "specguard-brief",
            Some("docs/DESIGN-spec-loop.md"),
            &["crates/specguard/src/coverage.rs"],
        )],
    );
    let (out, v) = brief_json(repo, "a of を !! -");
    check("no query token", &out, &v);
}

/// R3 acceptance 4 (exit-status half): a caller that branches only on the exit
/// status — a shell `if`, a pre-commit wrapper — must not read `undetermined`
/// as success. The status must also DISCRIMINATE: both real answers exit 0, so
/// a blanket non-zero would not satisfy this either.
#[test]
fn brief_json_undetermined_is_not_success_to_an_exit_status_only_caller() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    init_repo(repo);
    write_config(repo, "unused");
    let task = "make coverage.rs emit a tri-state verdict";

    let (undet, _) = brief_json(repo, task); // no store yet
    assert!(
        !undet.status.success(),
        "`if specguard brief --json ...; then` must NOT take the success branch \
         on undetermined (exit code was {:?})",
        undet.status.code()
    );
    assert_eq!(undet.status.code(), Some(10));

    write_map(
        repo,
        &[map_entry(
            "specguard-brief",
            Some("docs/DESIGN-spec-loop.md"),
            &["crates/specguard/src/coverage.rs"],
        )],
    );
    let (covered, _) = brief_json(repo, task);
    write_map(
        repo,
        &[map_entry(
            "specguard-brief",
            None,
            &["crates/specguard/src/coverage.rs"],
        )],
    );
    let (not_covered, _) = brief_json(repo, task);
    assert!(
        covered.status.success() && not_covered.status.success(),
        "both real answers must exit 0, else the non-zero on undetermined says \
         nothing (covered {:?}, not-covered {:?})",
        covered.status.code(),
        not_covered.status.code()
    );
    assert_ne!(
        undet.status.code(),
        covered.status.code(),
        "undetermined and covered must be distinguishable by status alone"
    );
}

/// R3 acceptance 3 (end-to-end half): for the SAME store state, the prose the
/// brief prompt carries and the machine `verdict` agree — in all three states,
/// including the entry list the `covered` prose quotes.
#[test]
fn brief_prose_and_json_verdict_agree_for_the_same_store_state() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    init_repo(repo);
    write_config(repo, "unused");
    let task = "make coverage.rs emit a tri-state verdict";

    // undetermined (no store)
    let (_, v) = brief_json(repo, task);
    assert_eq!(verdict_of(&v), prose_verdict(repo, task), "undetermined");

    // covered
    write_map(
        repo,
        &[map_entry(
            "specguard-brief",
            Some("docs/DESIGN-spec-loop.md"),
            &["crates/specguard/src/coverage.rs"],
        )],
    );
    let (_, v) = brief_json(repo, task);
    assert_eq!(verdict_of(&v), prose_verdict(repo, task), "covered");
    let prompt = run_specguard(repo, "HEAD", &["brief", task, "--prompt"]);
    let prose = String::from_utf8_lossy(&prompt.stdout).to_string();
    for e in v["entries"].as_array().unwrap() {
        assert!(
            prose.contains(e.as_str().unwrap()),
            "every entry the JSON credits must appear in the prose: {e} not in\n{prose}"
        );
    }

    // not-covered
    write_map(
        repo,
        &[map_entry(
            "specguard-brief",
            None,
            &["crates/specguard/src/coverage.rs"],
        )],
    );
    let (_, v) = brief_json(repo, task);
    assert_eq!(verdict_of(&v), prose_verdict(repo, task), "not-covered");
}

/// R3 acceptance 3, the structural half: ONE invocation of `brief` reads the
/// spec map exactly ONCE.
///
/// Agreement tests cannot see a duplicated resolver that happens to agree; a
/// read counter can. The store here is a FIFO carrying the map exactly once, so
/// a second resolution anywhere in the invocation blocks forever instead of
/// quietly agreeing with the first. The assertion is that the process finishes
/// (and that the one read it did make actually fed the prose).
#[test]
fn brief_reads_the_spec_map_exactly_once_per_invocation() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    init_repo(repo);
    write_config(repo, "unused");
    let task = "make coverage.rs emit a tri-state verdict";

    let body = format!(
        "last_synced = \"deadbeef\"\n{}",
        map_entry(
            "specguard-brief",
            Some("docs/DESIGN-spec-loop.md"),
            &["crates/specguard/src/coverage.rs"],
        )
    );
    let src = repo.join("map-body.toml");
    fs::write(&src, &body).unwrap();

    let fifo = repo.join(".specguard/spec-map.toml");
    fs::create_dir_all(fifo.parent().unwrap()).unwrap();
    let mk = Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .expect("mkfifo runs");
    assert!(mk.success(), "mkfifo failed — cannot count store reads");

    // One writer, one payload: the second reader (if any) gets no data and no
    // writer, and blocks.
    let mut writer = Command::new("bash")
        .arg("-c")
        .arg(format!("cat {} > {}", src.display(), fifo.display()))
        .spawn()
        .expect("writer spawns");

    let mut child = Command::new(env!("CARGO_BIN_EXE_specguard"))
        .current_dir(repo)
        .args(["--config", "specguard.toml", "--date", "2026-01-01"])
        .args(["brief", task, "--prompt"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("specguard spawns");

    // Poll rather than wait(): a second read would never return.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let finished = loop {
        match child.try_wait().expect("try_wait") {
            Some(_) => break true,
            None if std::time::Instant::now() >= deadline => break false,
            None => std::thread::sleep(std::time::Duration::from_millis(50)),
        }
    };
    if !finished {
        let _ = child.kill();
        let _ = child.wait();
        let _ = writer.kill();
        let _ = writer.wait();
        panic!(
            "`specguard brief` did not finish within 20s against a one-shot spec-map \
             FIFO: it read the store more than once, i.e. the coverage resolution is \
             not shared between the prose and the machine verdict"
        );
    }
    let out = child.wait_with_output().expect("output");
    let _ = writer.wait();
    assert!(
        out.status.success(),
        "brief --prompt failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        s.contains("**covered**") && s.contains("specguard-brief"),
        "the single read must be the one that fed the prose: {s}"
    );
}
