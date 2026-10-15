// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog e65acc45 — `fugu-router code-index build` reports `rebuilt: true`
//! (exit 0) even when `harness_core::code_index::write_index` / `write_meta`
//! could not write anything, because both drop every IO error.
//!
//! Fixture: a git repo with one tracked `.rs` file, where `<root>/.fugu` is a
//! regular FILE, so neither `.fugu/code-index.jsonl` nor its meta can be
//! created. The claimed property: a build whose writes failed must not report
//! success. The control pins that a writable root builds and reports rebuilt.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct TempRepo(PathBuf);
impl TempRepo {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "fugu-router-e65acc45-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            let st = Command::new("git")
                .args(args)
                .current_dir(&dir)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap();
            assert!(st.success(), "git {args:?}");
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(
            dir.join("lib.rs"),
            "pub fn alpha_helper() -> i32 {\n    0\n}\n",
        )
        .unwrap();
        git(&["add", "lib.rs"]);
        git(&["commit", "-q", "-m", "seed"]);
        TempRepo(dir)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for TempRepo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn build(root: &Path) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_fugu-router"))
        .args(["code-index", "build", "--root"])
        .arg(root)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn unwritable_index_dir_does_not_report_rebuilt() {
    let repo = TempRepo::new("unwritable");
    // `.fugu` as a plain file: create_dir_all / File::create under it fail.
    std::fs::write(repo.path().join(".fugu"), "not a directory").unwrap();

    let (code, stdout, stderr) = build(repo.path());
    assert!(
        !repo.path().join(".fugu/code-index.jsonl").exists(),
        "fixture: the index must really be unwritable"
    );
    let claims_success = code == 0 && stdout.contains("\"rebuilt\":true");
    assert!(
        !claims_success,
        "build wrote nothing yet reported success: exit={code} stdout={stdout} stderr={stderr}"
    );
}

/// Control: a writable root builds the index and reports rebuilt.
#[test]
fn writable_root_reports_rebuilt_and_writes_index() {
    let repo = TempRepo::new("writable");
    let (code, stdout, stderr) = build(repo.path());
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(stdout.contains("\"rebuilt\":true"), "stdout={stdout}");
    assert!(repo.path().join(".fugu/code-index.jsonl").exists());
}
