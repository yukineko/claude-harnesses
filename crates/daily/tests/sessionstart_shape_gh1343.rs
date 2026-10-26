//! GH 1343: Claude Code never delivers a top-level `additionalContext` from a
//! SessionStart hook. The real stdout of `daily session-start` must nest it
//! under `hookSpecificOutput` (hookEventName SessionStart); `systemMessage`
//! (when present) stays top-level.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

fn session_start(home: &Path) -> serde_json::Value {
    let mut child = Command::new(env!("CARGO_BIN_EXE_daily"))
        .arg("session-start")
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .current_dir(home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("daily spawns");
    let payload = format!(
        r#"{{"hook_event_name":"SessionStart","cwd":"{}"}}"#,
        home.display()
    );
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {stdout:?}"))
}

fn home(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("daily-gh1343-{tag}-{}", std::process::id()));
    std::fs::remove_dir_all(&d).ok();
    std::fs::create_dir_all(d.join(".daily")).unwrap();
    d
}

#[test]
fn summary_is_nested() {
    let h = home("summary");
    std::fs::write(
        h.join(".daily/config.toml"),
        "skip_when_driver_active = false\n",
    )
    .unwrap();
    let st = Command::new(env!("CARGO_BIN_EXE_daily"))
        .args(["add", "--name", "toucher", "--command", "true"])
        .env("HOME", &h)
        .current_dir(&h)
        .status()
        .unwrap();
    assert!(st.success());
    let v = session_start(&h);
    assert_eq!(
        v["hookSpecificOutput"]["hookEventName"], "SessionStart",
        "{v}"
    );
    assert!(v.get("additionalContext").is_none(), "{v}");
    let ctx = v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(ctx.contains("toucher (ok)"), "{ctx}");
    std::fs::remove_dir_all(&h).ok();
}

#[test]
fn unparseable_config_keeps_system_message_top_level_and_nests_context() {
    let h = home("badcfg");
    std::fs::write(h.join(".daily/config.toml"), "this is not = valid toml ][").unwrap();
    let v = session_start(&h);
    assert!(v["systemMessage"].as_str().is_some(), "{v}");
    assert!(v.get("additionalContext").is_none(), "{v}");
    assert_eq!(
        v["hookSpecificOutput"]["hookEventName"], "SessionStart",
        "{v}"
    );
    assert!(
        v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .is_some_and(|c| c.contains("UNKNOWN")),
        "{v}"
    );
    std::fs::remove_dir_all(&h).ok();
}
