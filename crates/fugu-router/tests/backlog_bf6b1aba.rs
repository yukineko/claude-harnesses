// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog bf6b1aba — once budgetguard is located, `fugu-router route` reads a
//! budgetguard spawn error, a non-zero exit, or unparseable `status --json`
//! stdout as "no pressure" (full-price routing = the permissive side).
//!
//! Claim (crates/fugu-router/src/budget.rs `under_pressure`): the
//! `Command::output()` Err arm, the `!out.status.success()` arm and
//! `parse_pressure`'s `unwrap_or(false)` all return `false`. Per CLAUDE.md §3 a
//! checker that could not answer is not a checker that said "fine", so each of
//! these must route as pressured (and say why). The control pins that a
//! budgetguard that genuinely answers `{"pressure":false}` routes unchanged.
#![cfg(unix)]

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const DECOMP: &str = r#"{"goal":"g","tasks":[{"id":"t1","title":"refactor the parser module","class":"refactor","touched_files":["src/parser.rs"]}]}"#;

struct TempDir(PathBuf);
impl TempDir {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!(
            "fugu-router-bf6b1aba-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Plant `<home>/.claude/plugins/cache/yukineko/budgetguard/0.1.0/bin/budgetguard`
/// with `body` and `mode`.
fn plant_budgetguard(home: &Path, body: &str, mode: u32) {
    let bin = home.join(".claude/plugins/cache/yukineko/budgetguard/0.1.0/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let p = bin.join("budgetguard");
    std::fs::write(&p, body).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// Run `fugu-router route --report <report>` under `home`. Returns (exit code,
/// stderr, report JSON).
fn route(home: &Path, report: &Path) -> (i32, String, serde_json::Value) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_fugu-router"))
        .args(["route", "--report"])
        .arg(report)
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .current_dir(home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("fugu-router spawns");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(DECOMP.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let rep = std::fs::read_to_string(report)
        .map(|s| serde_json::from_str(&s).unwrap())
        .unwrap_or(serde_json::Value::Null);
    (out.status.code().unwrap_or(-1), stderr, rep)
}

fn assert_pressured(case: &str, body: &str, mode: u32) {
    let tmp = TempDir::new(case);
    let home = tmp.path().join("home");
    plant_budgetguard(&home, body, mode);
    let report = tmp.path().join("report.json");
    let (code, stderr, rep) = route(&home, &report);
    assert_eq!(code, 0, "route must still succeed; stderr={stderr}");
    let rationale = rep["t1"]["rationale"].as_str().unwrap_or_default();
    assert!(
        stderr.contains("daily budget pressure") && rationale.contains("budget pressure"),
        "{case}: a located budgetguard that did not answer must route as pressured \
         (restrictive side), not as no-pressure full-price routing.\nstderr={stderr}\nreport={rep}"
    );
}

#[test]
#[ignore = "backlog bf6b1aba: RED repro"]
fn nonzero_exit_routes_as_pressured() {
    assert_pressured(
        "nonzero",
        "#!/bin/sh\necho 'budgetguard: boom' >&2\nexit 1\n",
        0o755,
    );
}

#[test]
#[ignore = "backlog bf6b1aba: RED repro"]
fn unparseable_status_json_routes_as_pressured() {
    assert_pressured(
        "garbage",
        "#!/bin/sh\necho 'not json at all'\nexit 0\n",
        0o755,
    );
}

#[test]
#[ignore = "backlog bf6b1aba: RED repro"]
fn spawn_error_routes_as_pressured() {
    // Present in the cache (so resolve() returns Known(Some)) but not
    // executable: Command::output() fails with PermissionDenied.
    assert_pressured(
        "spawnerr",
        "#!/bin/sh\necho '{\"pressure\":false}'\n",
        0o644,
    );
}

/// Control: a budgetguard that really answers "no pressure" routes unchanged.
#[test]
fn genuine_no_pressure_routes_unchanged() {
    let tmp = TempDir::new("control");
    let home = tmp.path().join("home");
    plant_budgetguard(&home, "#!/bin/sh\necho '{\"pressure\":false}'\n", 0o755);
    let report = tmp.path().join("report.json");
    let (code, stderr, rep) = route(&home, &report);
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(!stderr.contains("daily budget pressure"), "stderr={stderr}");
    assert!(
        !rep["t1"]["rationale"]
            .as_str()
            .unwrap_or_default()
            .contains("budget pressure"),
        "report={rep}"
    );
}
