// Reproduction test for backlog 84383678 (audit shard s10-small-b).
// RED while the item is an open defect.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn project(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pg-s10-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "t@t"],
        vec!["config", "user.name", "t"],
    ] {
        let _ = Command::new("git").current_dir(&dir).args(&args).output();
    }
    std::fs::write(dir.join("a.rs"), "fn f() { panic!() }\n").unwrap();
    std::fs::write(
        dir.join("propguard.toml"),
        "done_criteria = \"idempotent; never panic; stable output schema\"\n",
    )
    .unwrap();
    dir
}

fn run(dir: &Path, args: &[&str], stdin: &str) -> (String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_propguard"))
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("PROPGUARD_STATE_DIR", dir.join(".propguard-state"))
        .env("CLAUDE_CODE_SESSION_ID", "s10-sess")
        .env_remove("HARNESS_TRUST_ALL")
        .env_remove("PROPGUARD_CRITERIA")
        .env_remove("PROPGUARD_DISABLE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn stop(dir: &Path) -> (String, String) {
    let payload = serde_json::json!({
        "session_id": "s10-sess",
        "cwd": dir.to_string_lossy(),
        "hook_event_name": "Stop",
    })
    .to_string();
    run(dir, &["check"], &payload)
}

fn blocked(stdout: &str) -> bool {
    stdout.contains("\"decision\": \"block\"") || stdout.contains("\"decision\":\"block\"")
}

/// 84383678: an UNTRUSTED project propguard.toml carrying done_criteria is
/// "there is an operator config we could not honour", not "no config". The Stop
/// must not resolve to allow(no-criteria). Control: the same project, once
/// trusted, blocks (so the apparatus can block).
#[test]
#[ignore = "backlog 84383678: open defect, remove ignore when fixed"]
fn backlog_84383678_untrusted_project_config_does_not_allow_stop() {
    // Control: trusted -> blocks.
    let trusted = project("trusted");
    let (out, err) = run(&trusted, &["trust"], "");
    eprintln!("trust: {out} {err}");
    let (cout, cerr) = stop(&trusted);
    assert!(
        blocked(&cout),
        "apparatus control failed (trusted project must block); stdout={cout:?} stderr={cerr:?}"
    );

    // Subject: untrusted -> must not silently allow.
    let untrusted = project("untrusted");
    let (uout, uerr) = stop(&untrusted);
    eprintln!("untrusted stop: stdout={uout:?} stderr={uerr:?}");
    assert!(
        blocked(&uout),
        "untrusted propguard.toml with done_criteria fell back to defaults and the stop was \
         allowed; stdout={uout:?} stderr={uerr:?}"
    );
}
