// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog abba6f0d — `fugu-router route` when budgetguard's presence cannot be
//! determined.
//!
//! "No budget pressure" is the permissive answer of `budget::under_pressure`
//! (full-price models, no downgrade). A plugin cache dir for budgetguard that
//! exists but cannot be read means we could not tell whether a budget is being
//! enforced, so routing must treat it as pressure and say why on stderr.
//! Observed RED at `82ee4a78` (the old resolver's `read_dir(..).ok()?` turned
//! the unreadable dir into "budgetguard absent" → no pressure) and GREEN after
//! `27824792`. The control pins that an observed absence still routes unchanged.
#![cfg(unix)]

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const DECOMP: &str = r#"{"goal":"g","tasks":[{"id":"t1","title":"refactor the parser module","class":"refactor","touched_files":["src/parser.rs"]}]}"#;

/// Restores a dir's mode on drop so the tempdir can always be removed.
struct RestoreMode(PathBuf);
impl Drop for RestoreMode {
    fn drop(&mut self) {
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

/// A unique temp dir (fugu-router has no `tempfile` dev-dependency), removed on drop.
struct TempDir(PathBuf);
impl TempDir {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!(
            "fugu-router-vabba-{tag}-{}-{nanos}",
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

/// Run `fugu-router route --report <report>` under `home`, with a PATH that
/// holds no budgetguard. Returns (exit code, stderr, report JSON).
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

fn rationale(rep: &serde_json::Value) -> String {
    rep["t1"]["rationale"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[test]
fn unreadable_budgetguard_cache_dir_routes_as_pressured() {
    let tmp = TempDir::new("unreadable");
    let home = tmp.path().join("home");
    let bg = home.join(".claude/plugins/cache/yukineko/budgetguard");
    std::fs::create_dir_all(bg.join("0.1.0/bin")).unwrap();
    std::fs::set_permissions(&bg, std::fs::Permissions::from_mode(0o000)).unwrap();
    let _restore = RestoreMode(bg.clone());
    if std::fs::read_dir(&bg).is_ok() {
        eprintln!("SKIP: chmod 000 does not deny access here (root?)");
        return;
    }

    let report = tmp.path().join("report.json");
    let (code, stderr, rep) = route(&home, &report);
    assert_eq!(code, 0, "route must still succeed; stderr={stderr}");
    assert!(
        stderr.contains("could not locate budgetguard"),
        "the undetermined lookup must be named on stderr: {stderr}"
    );
    assert!(
        stderr.contains("daily budget pressure"),
        "an undetermined budgetguard must route as pressured (restrictive side): {stderr}"
    );
    assert!(
        rationale(&rep).contains("budget pressure"),
        "the routed decision must carry the budget downgrade: {rep}"
    );
}

#[test]
fn absent_budgetguard_routes_unchanged() {
    let tmp = TempDir::new("absent");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();

    let report = tmp.path().join("report.json");
    let (code, stderr, rep) = route(&home, &report);
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(
        !stderr.contains("daily budget pressure"),
        "budgetguard observed absent is no pressure: {stderr}"
    );
    assert!(
        !rationale(&rep).contains("budget pressure"),
        "no downgrade expected when budgetguard is absent: {rep}"
    );
}
