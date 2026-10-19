#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro: the tdd Stop hook treats unparseable stdin as an interactive CLI
//! run. In a repo that has implementation without a test, the verdict is a
//! violation, but with malformed stdin it is printed to stderr and the process
//! exits 1 with NO `decision:block` on stdout, which Claude Code reads as a
//! non-blocking error, i.e. the stop is allowed. (integration.rs pins the
//! empty-cwd case only; that test is left untouched.)

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

fn run_gate(cwd: &Path, home: &Path, path: &Path, payload: &str) -> (i32, String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_tdd"))
        .arg("gate")
        .current_dir(cwd)
        .env("HOME", home)
        .env("PATH", path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary spawns");
    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(payload.as_bytes());
    }
    let out = child.wait_with_output().expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
#[ignore = "RED repro: tdd gate allows the stop on malformed stdin in a violating repo"]
fn malformed_stdin_in_violating_repo_does_not_allow() {
    let root = std::env::temp_dir().join(format!("tdd-malformed-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let empty_bin = root.join("empty-bin");
    let home = root.join("home");
    let work = root.join("work");
    std::fs::create_dir_all(&empty_bin).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(work.join(".git")).unwrap();
    std::fs::create_dir_all(work.join("src")).unwrap();
    std::fs::write(work.join("src/lib.rs"), "pub fn f() -> u8 { 1 }\n").unwrap();

    let valid = format!(
        r#"{{"hook_event_name":"Stop","session_id":"mal-ctl","stop_hook_active":false,"cwd":{}}}"#,
        serde_json::to_string(&work.to_string_lossy()).unwrap()
    );
    // Control: a well-formed payload on the same repo blocks.
    let (_, ctl_out, _) = run_gate(&work, &home, &empty_bin, &valid);
    assert!(
        ctl_out.contains("\"decision\":\"block\""),
        "control must block, else this repro proves nothing: {ctl_out:?}"
    );

    let (code, stdout, stderr) = run_gate(&work, &home, &empty_bin, "not json");
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        stdout.contains("\"decision\":\"block\"") || code == 2,
        "malformed stdin on a violating repo must not allow the stop. exit={code} stdout={stdout:?} stderr={stderr:?}"
    );
}
