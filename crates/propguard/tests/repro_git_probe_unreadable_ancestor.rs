#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(unix)]
//! RED repro: `git_probe` decides NotRepo from `Path::exists()`. With the repo
//! root unsearchable (mode 000), both the `.git` stat and git's chdir fail, so
//! the probe sees "no git, no .git" = NotRepo and propguard allows ("no-git")
//! instead of treating the change set as undetermined.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Restore(PathBuf);
impl Drop for Restore {
    fn drop(&mut self) {
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

fn run_check(cwd: &Path, home: &Path, payload_cwd: &Path, sid: &str) -> (i32, String) {
    let payload = serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": sid,
        "stop_hook_active": false,
        "cwd": payload_cwd.to_string_lossy(),
    })
    .to_string();
    let mut child = Command::new(env!("CARGO_BIN_EXE_propguard"))
        .arg("check")
        .current_dir(cwd)
        .env("HOME", home)
        // Hermetic session: the repeat ledger keys on CLAUDE_CODE_SESSION_ID,
        // so the parent's value leaking in made the control's block and this
        // run's identical block look like one session's repeat (waived). In a
        // real hook the env var and the payload session agree.
        .env("CLAUDE_CODE_SESSION_ID", sid)
        .env("PROPGUARD_STATE_DIR", home.join(".propguard-state"))
        .env(
            "PROPGUARD_CRITERIA",
            "idempotent; never panic; stable output schema",
        )
        .env_remove("PROPGUARD_DISABLE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary spawns");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn unsearchable_repo_root_does_not_read_as_not_a_repo() {
    let base = std::env::temp_dir().join(format!("pg-unreadable-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let home = base.join("home");
    let repo = base.join("repo");
    let work = repo.join("work");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("a.rs"), "fn f() { panic!() }\n").unwrap();

    // Control: the same layout, readable, blocks (git rejects the empty .git
    // while the filesystem corroborates it => Undetermined => block).
    let (_, ctl) = run_check(&base, &home, &work, "ctl");
    assert!(
        ctl.contains("\"decision\":\"block\""),
        "control must block, else this repro proves nothing: {ctl:?}"
    );
    let _restore = Restore(repo.clone());
    std::fs::set_permissions(&repo, std::fs::Permissions::from_mode(0o000)).unwrap();
    if work.exists() {
        eprintln!("SKIP: mode 000 does not deny access here (root?)");
        return;
    }
    let (code, stdout) = run_check(&base, &home, &work, "restricted");
    drop(_restore);
    let _ = std::fs::remove_dir_all(&base);
    assert_eq!(code, 0, "Stop hook exits 0; stdout: {stdout}");
    assert!(
        stdout.contains("\"decision\":\"block\""),
        "a repo root that cannot be read is UNDETERMINED, not 'not a repo'; propguard allowed. stdout: {stdout:?}"
    );
}
