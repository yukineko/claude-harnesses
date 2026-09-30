#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Hit 4 (stuckguard main.rs:127): AnchorLookup::Undetermined => None.
//! Force: HOME sandbox whose plugin-cache overwatch dir is chmod 000 and a PATH
//! with no `overwatch`, so resolve_overwatch_binary is Undetermined.
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};

fn run_watch(cache_perm: Option<u32>) -> (i32, String, String) {
    let home =
        std::env::temp_dir().join(format!("sg-anchor-{}-{:?}", std::process::id(), cache_perm));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let cache = home.join(".claude/plugins/cache/yukineko/overwatch");
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::write(
        home.join("stuckguard.toml"),
        "heartbeat_piggyback_enabled = true\nscope_drift_enabled = true\n",
    )
    .unwrap();
    if let Some(m) = cache_perm {
        std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(m)).unwrap();
    }
    let empty = home.join("emptybin");
    std::fs::create_dir_all(&empty).unwrap();
    let mut c = Command::new(env!("CARGO_BIN_EXE_stuckguard"))
        .arg("watch")
        .current_dir(&home)
        .env("HOME", &home)
        .env("PATH", &empty)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    c.stdin
        .take()
        .unwrap()
        .write_all(br#"{"hook_event_name":"PostToolUse","session_id":"s1","tool_name":"Edit","tool_input":{"file_path":"a.rs","old_string":"a","new_string":"b"}}"#)
        .unwrap();
    let o = c.wait_with_output().unwrap();
    if cache_perm.is_some() {
        let _ = std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o755));
    }
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

#[test]
fn undetermined_anchor_is_labelled_not_silent() {
    let (code, out, err) = run_watch(Some(0o000));
    eprintln!("UNDET code={code} stdout={out:?} stderr={err:?}");
    assert_eq!(code, 0);
    assert!(
        err.contains("could not query overwatch") && err.contains("skipped"),
        "undetermined must be diagnosed: {err:?}"
    );
}

#[test]
fn control_no_overwatch_installed_is_silent_nolease() {
    let (code, out, err) = run_watch(None);
    eprintln!("CONTROL code={code} stdout={out:?} stderr={err:?}");
    assert_eq!(code, 0);
    assert!(!err.contains("could not query overwatch"), "{err:?}");
}
