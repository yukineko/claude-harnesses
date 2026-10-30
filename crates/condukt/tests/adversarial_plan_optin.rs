// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `condukt adversarial plan` is OPT-IN only (user ruling, ROI waste
//! reduction): touching a GATE crate no longer forces the panel. The panel
//! engages only when the global switch is on (`[adversarial] enabled` in
//! config.toml, or `CONDUKT_ADVERSARIAL=1`). `high_stakes` is still computed
//! and echoed in the plan JSON.
//!
//! Drives the real binary with an isolated `$HOME` (no config.toml) and the
//! `CONDUKT_ADVERSARIAL` variable removed from the inherited environment.

use std::path::PathBuf;
use std::process::{Command, Output};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

struct Home(PathBuf);

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn home(tag: &str) -> Home {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "condukt-adv-optin-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    Home(p)
}

fn plan(h: &Home, adversarial_env: Option<&str>) -> Output {
    let mut cmd = Command::new(bin());
    harness_core::git_env::scrub(&mut cmd);
    cmd.args([
        "adversarial",
        "plan",
        "--touched",
        "crates/blastguard/src/main.rs",
    ])
    .current_dir(&h.0)
    .env("HOME", &h.0)
    .env_remove("CONDUKT_ADVERSARIAL");
    if let Some(v) = adversarial_env {
        cmd.env("CONDUKT_ADVERSARIAL", v);
    }
    cmd.output().expect("spawn condukt")
}

fn json(out: &Output) -> serde_json::Value {
    let s = String::from_utf8_lossy(&out.stdout);
    let line = s
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'))
        .unwrap_or_else(|| panic!("no JSON on stdout: {s:?} stderr: {:?}", out.stderr));
    serde_json::from_str(line).unwrap()
}

#[test]
fn gate_crate_touch_does_not_engage_panel_when_switch_off() {
    let h = home("off");
    let out = plan(&h, None);
    let v = json(&out);
    assert_eq!(
        out.status.code(),
        Some(1),
        "switch off must take the single-verifier path (exit 1): {v}"
    );
    assert_eq!(v["engaged"], serde_json::json!(false), "{v}");
    assert_eq!(v["high_stakes"], serde_json::json!(true), "{v}");
    assert_eq!(v["size"], serde_json::json!(1), "{v}");
}

#[test]
fn env_switch_engages_panel() {
    let h = home("on");
    let out = plan(&h, Some("1"));
    let v = json(&out);
    assert_eq!(
        out.status.code(),
        Some(0),
        "switch on must engage (exit 0): {v}"
    );
    assert_eq!(v["engaged"], serde_json::json!(true), "{v}");
    assert_eq!(v["high_stakes"], serde_json::json!(true), "{v}");
}

#[test]
fn config_switch_engages_panel() {
    let h = home("cfg");
    let dir = h.0.join(".condukt");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("config.toml"), "[adversarial]\nenabled = true\n").unwrap();
    let out = plan(&h, None);
    let v = json(&out);
    assert_eq!(out.status.code(), Some(0), "config switch must engage: {v}");
    assert_eq!(v["engaged"], serde_json::json!(true), "{v}");
}
