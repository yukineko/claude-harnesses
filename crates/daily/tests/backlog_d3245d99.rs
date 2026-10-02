//! Repro for backlog d3245d99: `load_config` maps EVERY read error to
//! `Config::default()` (enabled, no tasks, built-in security task).
//!
//! Setup: `~/.daily/config.toml` exists but cannot be read as a file (it is a
//! directory -> EISDIR, not NotFound). `daily list` must not present this as the
//! healthy default ("enabled, 0 registered"); it must say the config is unknown /
//! unreadable or exit non-zero.

use std::process::Command;

#[test]
#[ignore = "backlog d3245d99: open defect, remove ignore when fixed"]
fn unreadable_config_is_not_reported_as_default_enabled() {
    let home = std::env::temp_dir().join(format!("daily-d3245d99-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join(".daily").join("config.toml")).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_daily"))
        .arg("list")
        .env("HOME", &home)
        .current_dir(&home)
        .output()
        .expect("daily runs");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let _ = std::fs::remove_dir_all(&home);
    let reported_default = stdout.contains("daily tasks (enabled, 0 registered)");
    assert!(
        !reported_default || !out.status.success(),
        "unreadable config silently degraded to default.\nexit={:?}\nstdout={stdout}\nstderr={stderr}",
        out.status.code()
    );
}
