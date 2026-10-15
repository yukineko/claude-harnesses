// このファイルは丸ごと integration test なので unwrap/expect を許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Independent-verifier tests (not the implementer's): since backlog fd7ad41f
//! (2026-10-04) `backlog add` files a task ONLY when `--repro-test` reproduces
//! at HEAD; otherwise it exits 2 and files nothing. These tests pin what
//! `overwatch review-queue --to-backlog` does about that.
//!
//! The fake `backlog` here emulates exactly that contract: `add` WITHOUT a
//! `--repro-test` argument exits 2 and files nothing; with one it "files" (exit
//! 0, line appended to $FAKE_FILED). It does NOT run the repro test, so these
//! tests observe what the bridge PASSES, not the real backlog's judgement.
//!
//! H1: a finding whose `#[ignore = "<finding-id>: ..."]` repro test exists and
//!     genuinely fails must end up as a filed task (bridge must pass it as
//!     `--repro-test`).
//! H2: when `backlog add` refuses, the bridge must not look identical to
//!     success (exit 0, no distinguishing signal in stdout).
#![cfg(unix)]
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_overwatch")
}

struct Env {
    root: PathBuf,
    real_home: String,
}

/// One shared HOME for the whole test binary (the overwatch lib resolves its
/// store root from the PROCESS HOME when seeding findings).
fn env() -> &'static Env {
    static E: OnceLock<Env> = OnceLock::new();
    E.get_or_init(|| {
        let real_home = std::env::var("HOME").unwrap_or_default();
        let root = std::env::temp_dir().join(format!("ow-repro-evidence-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("home")).unwrap();
        std::env::set_var("HOME", root.join("home"));
        Env { root, real_home }
    })
}

fn write_fake_backlog(dir: &Path) {
    fs::create_dir_all(dir).unwrap();
    let script = dir.join("backlog");
    let body = r#"#!/bin/sh
if [ "$1" = "add" ]; then
  echo "$*" >> "$FAKE_ADD_LOG"
  case "$*" in
    *--repro-test*) echo "$*" >> "$FAKE_FILED"; echo "added: fake"; exit 0 ;;
    *) echo "refused: no --repro-test (exit 2), recorded unproven" >&2; exit 2 ;;
  esac
fi
exit 0
"#;
    fs::write(&script, body).unwrap();
    let mut p = fs::metadata(&script).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&script, p).unwrap();
}

fn lines(p: &Path) -> Vec<String> {
    fs::read_to_string(p)
        .map(|t| t.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

fn seed_confirmed(project: &Path, id: &str, summary: &str) {
    let f = overwatch::review_finding::ReviewFinding::new(
        id.into(),
        "continuous-audit".into(),
        Some("high".into()),
        summary.into(),
        Some("src/foo.rs".into()),
        None,
        100,
    )
    .with_verdict(overwatch::review_finding::AuditVerdict::Confirmed);
    overwatch::store::append_review_finding(project, &f).unwrap();
}

struct Run {
    out: std::process::Output,
    add_log: PathBuf,
    filed: PathBuf,
}

fn run_bridge(project: &Path, tag: &str) -> Run {
    let e = env();
    let bindir = e.root.join(format!("bin-{tag}"));
    write_fake_backlog(&bindir);
    let add_log = e.root.join(format!("add-{tag}.log"));
    let filed = e.root.join(format!("filed-{tag}.log"));
    let path = format!(
        "{}:{}",
        bindir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::new(bin())
        .args(["review-queue", "--to-backlog"])
        .current_dir(project)
        .env("HOME", e.root.join("home"))
        .env("PATH", path)
        .env("RUSTUP_HOME", format!("{}/.rustup", e.real_home))
        .env("CARGO_HOME", format!("{}/.cargo", e.real_home))
        .env("CARGO_TARGET_DIR", e.root.join(format!("target-{tag}")))
        .env("FAKE_ADD_LOG", &add_log)
        .env("FAKE_FILED", &filed)
        .env_remove("OVERWATCH_BACKLOG_BIN")
        .output()
        .expect("spawn overwatch");
    Run {
        out,
        add_log,
        filed,
    }
}

/// H1: a real, genuinely-failing `#[ignore = "<finding-id>: ..."]` repro test
/// exists for the finding. The CORRECT behaviour is that the bridge hands it to
/// `backlog add --repro-test ...` so a task is filed.
#[test]
fn confirmed_finding_with_failing_ignored_repro_test_is_filed_with_repro_test() {
    let e = env();
    let project = e.root.join("h1-project");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"h1fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    )
    .unwrap();
    fs::write(
        project.join("src/lib.rs"),
        "#[test]\n#[ignore = \"F-H1-1: repro of the bug\"]\nfn repro_h1_bug() {\n    assert_eq!(1, 2, \"bug present\");\n}\n",
    )
    .unwrap();
    seed_confirmed(&project, "F-H1-1", "h1 confirmed finding with repro");

    let r = run_bridge(&project, "h1");
    let stderr = String::from_utf8_lossy(&r.out.stderr);
    let adds = lines(&r.add_log);
    assert_eq!(adds.len(), 1, "one add attempted; stderr={stderr}");
    assert!(
        adds[0].contains("--repro-test"),
        "the add must carry --repro-test (the found ignored test); args were: {}\nstderr={stderr}",
        adds[0]
    );
    assert_eq!(
        lines(&r.filed).len(),
        1,
        "a task must be filed for the finding; args were: {}\nstderr={stderr}",
        adds[0]
    );
}

/// H2: every `backlog add` is refused (exit 2). The bridge must not be
/// indistinguishable from a fully successful run: either a non-zero exit or an
/// explicit failure count in the stdout JSON summary.
#[test]
fn refused_backlog_add_is_distinguishable_from_success() {
    let e = env();
    let project = e.root.join("h2-project");
    fs::create_dir_all(&project).unwrap();
    seed_confirmed(
        &project,
        "F-H2-1",
        "h2 confirmed finding, add will be refused",
    );

    let r = run_bridge(&project, "h2");
    let stdout = String::from_utf8_lossy(&r.out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&r.out.stderr).to_string();
    assert_eq!(lines(&r.add_log).len(), 1, "add attempted; stderr={stderr}");
    assert_eq!(lines(&r.filed).len(), 0, "nothing filed (fake refused)");

    let json: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout must be a JSON summary ({e}): {stdout:?}"));
    let has_failure_signal = [
        "failed",
        "refused",
        "add_failed",
        "unproven",
        "skipped_failed",
    ]
    .iter()
    .any(|k| {
        json.get(*k)
            .map(|v| v != &serde_json::json!(0) && !v.is_null())
            .unwrap_or(false)
    });
    assert!(
        !r.out.status.success() || has_failure_signal,
        "refused add is indistinguishable from success: exit={:?} stdout={stdout} stderr={stderr}",
        r.out.status.code()
    );
}
