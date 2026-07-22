// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! FAULT INJECTION — C4: `specguard::report::sentinel_pending` answers with
//! `Path::exists()`, which returns `false` for BOTH "no sentinel is raised" and
//! "I am not allowed to look".
//!
//! `crates/specguard/src/report.rs:55-57` (verbatim):
//! ```ignore
//! /// Whether a sentinel is currently raised (pending human review).
//! pub fn sentinel_pending(paths: &Paths) -> bool {
//!     paths.sentinel.exists()
//! }
//! ```
//! `Path::exists()` is documented as returning `false` if the metadata call
//! errors for ANY reason, permission denied included — so an unsearchable
//! parent directory turns a raised sentinel into "no sentinel".
//!
//! Consumer under test — `crates/specguard/src/main.rs:975-984`:
//! ```ignore
//! } else if report::sentinel_pending(paths) {
//!     // Clean this run, but a prior run's sentinel is still unhandled: keep the
//!     // baseline put so its drift stays in scope. Leave the sentinel untouched.
//!     ...
//! } else {
//!     // Fully clean: advance the baseline.
//!     report::advance_baseline(paths, &head)?;
//! ```
//! Advancing the baseline moves unfixed drift OUT of the next run's diff — the
//! sentinel exists precisely to stop that until a human `ack`s it.
//!
//! specguard is `[[bin]]`-only (no `[lib]`), so `report::sentinel_pending` is
//! not reachable from `tests/`. Making it reachable would be a production
//! change, which is forbidden here. Both tests therefore drive the real
//! binaries' decision paths end-to-end, using the `ingest` subcommand (the
//! agent-free counterpart of `run`) so no LLM is involved.
//!
//! The injected fault is deliberately narrow: only the sentinel's OWN parent
//! directory is chmod'ed 0o000. The config, the repo, the report dir and
//! `.last-ref` all stay readable, so nothing but `sentinel_pending`'s
//! `exists()` can account for the behaviour change.

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

/// `[output].sentinel` lives in its OWN subdirectory so the fault can be
/// aimed at exactly that directory and nothing else.
const SENTINEL_REL: &str = "sentinel-dir/.specguard-pending";

fn write_config(repo: &Path) {
    let cfg = format!(
        r#"
[project]
name = "Demo"
root = "."

[agent]
command = "bash"
args = ["-c", "true"]

[output]
report_dir = "reports"
sentinel = "{SENTINEL_REL}"

[[area]]
name = "src"
globs = ["src/**"]
canon = ["docs/spec.md"]
"#
    );
    fs::write(repo.join("specguard.toml"), cfg).unwrap();
}

fn specguard(repo: &Path, baseline: &str, sub: &[&str], stdin_data: &str) -> std::process::Output {
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
    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(stdin_data.as_bytes());
    }
    child.wait_with_output().expect("specguard runs")
}

/// Build a repo with a RAISED sentinel, returning `(baseline, shard_label)`.
fn repo_with_raised_sentinel(repo: &Path) -> (String, String) {
    let base = init_repo(repo);
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(repo.join("src/main.rs"), "fn main() {}\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "add src"]);
    write_config(repo);

    let pj = specguard(repo, &base, &["prompt", "--json"], "");
    assert!(
        pj.status.success(),
        "prompt --json stderr: {}",
        String::from_utf8_lossy(&pj.stderr)
    );
    let env: serde_json::Value = serde_json::from_slice(&pj.stdout).expect("valid JSON envelope");
    let label = env["shards"][0]["label"].as_str().unwrap().to_string();

    // Run 1: findings -> sentinel raised, baseline HELD.
    let dirty = serde_json::json!({
        "shards": [{
            "label": label,
            "stdout": "# audit\n\nreal drift\n\n<<<SPEC_AUDIT>>>\nneeds_user: yes\nsummary: unfixed drift",
            "code": 0
        }]
    })
    .to_string();
    let ing = specguard(repo, &base, &["ingest"], &dirty);
    assert!(
        ing.status.success() || ing.status.code() == Some(7),
        "ingest(dirty) stderr: {}",
        String::from_utf8_lossy(&ing.stderr)
    );
    assert!(
        repo.join(SENTINEL_REL).exists(),
        "precondition: the sentinel must be raised"
    );
    assert!(
        !repo.join("reports/.last-ref").exists(),
        "precondition: findings hold the baseline"
    );
    (base, label)
}

/// C4 at the DECISION level: a clean follow-up run with an unreachable sentinel
/// takes the `else` branch and advances the baseline, retiring drift no human
/// ever reviewed.
#[cfg(unix)]
#[test]
fn unsearchable_sentinel_dir_lets_a_clean_run_advance_the_baseline() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let (base, label) = repo_with_raised_sentinel(repo);

    let clean = serde_json::json!({
        "shards": [{
            "label": label,
            "stdout": "# audit\n\nnothing new\n\n<<<SPEC_AUDIT>>>\nneeds_user: no\nsummary: clean",
            "code": 0
        }]
    })
    .to_string();

    // ---- CONTROL: sentinel reachable. The prior sentinel must HOLD the baseline.
    let control = specguard(repo, &base, &["ingest"], &clean);
    let control_out = String::from_utf8_lossy(&control.stdout).into_owned();
    assert!(
        !repo.join("reports/.last-ref").exists(),
        "control precondition: an unhandled sentinel must hold the baseline; \
         stdout: {control_out}"
    );

    // ---- FAULT: make ONLY the sentinel's parent dir unsearchable.
    let sdir = repo.join("sentinel-dir");
    let mut perms = fs::metadata(&sdir).unwrap().permissions();
    perms.set_mode(0o000);
    fs::set_permissions(&sdir, perms).unwrap();

    let faulted = specguard(repo, &base, &["ingest"], &clean);
    let faulted_out = String::from_utf8_lossy(&faulted.stdout).into_owned();
    let last_ref = repo.join("reports/.last-ref");
    let advanced = last_ref.exists();
    let advanced_to = fs::read_to_string(&last_ref).unwrap_or_default();

    // Restore BEFORE asserting so a failing assert never leaves an undeletable
    // tempdir (mirrors specguard/src/decision.rs:183-203).
    let mut restore = fs::metadata(&sdir).unwrap().permissions();
    restore.set_mode(0o700);
    let _ = fs::set_permissions(&sdir, restore);

    assert!(
        !advanced,
        "FAIL-OPEN: the sentinel file is still on disk (a human has NOT reviewed \
         the drift), but its parent dir was unsearchable, so \
         `sentinel_pending()`'s `Path::exists()` returned false and specguard \
         took the 'fully clean' branch: advance_baseline() wrote \
         reports/.last-ref = {advanced_to:?}. The unfixed drift is now outside \
         the next run's diff and will never be re-detected.\n\
         control stdout: {control_out}\n\
         faulted stdout: {faulted_out}"
    );
}

/// C4 at the SessionStart-hook level: `specguard pending` is the surface that
/// tells a human "there is drift waiting for you". With the sentinel dir
/// unsearchable it prints nothing and exits 0 — the same output as a repo with
/// no pending findings at all.
#[cfg(unix)]
#[test]
fn unsearchable_sentinel_dir_silences_the_pending_offer() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let (base, _label) = repo_with_raised_sentinel(repo);

    // ---- CONTROL: reachable sentinel -> the offer block is printed.
    let control = specguard(repo, &base, &["pending"], "");
    let control_out = String::from_utf8_lossy(&control.stdout).into_owned();
    assert!(
        control_out.contains("specguard"),
        "control precondition: a raised sentinel must produce a fix-offer block, \
         got {control_out:?}"
    );

    // ---- FAULT.
    let sdir = repo.join("sentinel-dir");
    let mut perms = fs::metadata(&sdir).unwrap().permissions();
    perms.set_mode(0o000);
    fs::set_permissions(&sdir, perms).unwrap();

    let faulted = specguard(repo, &base, &["pending"], "");
    let code = faulted.status.code().unwrap_or(-1);
    let faulted_out = String::from_utf8_lossy(&faulted.stdout).into_owned();

    let mut restore = fs::metadata(&sdir).unwrap().permissions();
    restore.set_mode(0o700);
    let _ = fs::set_permissions(&sdir, restore);

    assert!(
        faulted_out.contains("specguard"),
        "FAIL-OPEN: `specguard pending` (the SessionStart hook surface) printed \
         NOTHING and exited {code} while a sentinel was raised on disk, because \
         its parent dir was unsearchable. 'I could not check' was rendered as \
         'nothing is pending'. control stdout was: {control_out:?}"
    );
}
