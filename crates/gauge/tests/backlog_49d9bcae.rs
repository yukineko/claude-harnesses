#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Repro for backlog 49d9bcae (P12 + P13; P14 is documented behaviour — see
//! the module note at the bottom).
//!
//! P12: `gauge install` resolves settings as
//! `dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))`, so with no
//! resolvable home it writes `./.claude/settings.json` in the CURRENT directory
//! and prints "Installed Stop hook" anyway (same shape as budgetguard
//! a1cc21f2). Induced on macOS by unsetting HOME and running under a
//! `sandbox-exec` profile that denies the opendirectoryd libinfo lookup (so the
//! passwd fallback fails) and denies every write under the real home.
//!
//! P13: `window::load` maps "no window registered" and "window.json is
//! corrupt" to the same `None`, so `gauge config show` tells the user nothing
//! is registered when their registration is in fact unreadable.
//!
//! P14 (not tested here): `find_transcript(None)` picking the newest transcript
//! across all projects is documented in its docstring, and condukt's only
//! consumer matches by exact agent_id, so a cross-project pick degrades to a
//! miss rather than a mis-attributed cost.

use std::path::Path;
use std::process::Command;

fn gauge(home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_gauge"))
        .args(args)
        .env("HOME", home)
        .current_dir(home)
        .output()
        .expect("spawn gauge")
}

// ── P13 ──────────────────────────────────────────────────────────────────────

/// Control: a valid registration is shown, and "missing" prints the
/// not-registered line — so the comparison below reads real outputs.
#[test]
fn control_window_show_reads_valid_and_missing() {
    let missing = tempfile::tempdir().unwrap();
    let out = gauge(missing.path(), &["config", "show"]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("no window registered"));

    let valid = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(valid.path().join(".gauge")).unwrap();
    std::fs::write(
        valid.path().join(".gauge/window.json"),
        r#"{"hours":5.0,"last_reset":"2026-10-01T00:00:00Z"}"#,
    )
    .unwrap();
    let out = gauge(valid.path(), &["config", "show"]);
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("hours:"),
        "valid window.json not read from $HOME/.gauge: {out:?}"
    );
}

#[test]
#[ignore = "backlog 49d9bcae: open defect (P13), remove ignore when fixed"]
fn corrupt_window_registration_is_not_reported_as_unregistered() {
    let missing = tempfile::tempdir().unwrap();
    let corrupt = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(corrupt.path().join(".gauge")).unwrap();
    std::fs::write(corrupt.path().join(".gauge/window.json"), "{\"hours\": 5, ").unwrap();

    let a = gauge(missing.path(), &["config", "show"]);
    let b = gauge(corrupt.path(), &["config", "show"]);
    assert!(
        (a.stdout, a.status.code()) != (b.stdout.clone(), b.status.code()),
        "a corrupt window.json is reported exactly like no registration: {:?}",
        String::from_utf8_lossy(&b.stdout)
    );
}

// ── P12 ──────────────────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
fn sandboxed(cwd: &Path, prog: &str, args: &[&str]) -> std::process::Output {
    assert!(
        Path::new("/usr/bin/sandbox-exec").exists(),
        "sandbox-exec missing: home_dir()==None cannot be induced (undetermined, not clean)"
    );
    let real_home = std::env::var("HOME").expect("HOME set for the test runner");
    let profile = format!(
        "(version 1)(allow default)\
         (deny mach-lookup (global-name \"com.apple.system.opendirectoryd.libinfo\"))\
         (deny file-write* (subpath \"{real_home}\"))"
    );
    Command::new("/usr/bin/sandbox-exec")
        .arg("-p")
        .arg(profile)
        .arg(prog)
        .args(args)
        .current_dir(cwd)
        .env_remove("HOME")
        .output()
        .expect("spawn sandbox-exec")
}

#[cfg(target_os = "macos")]
#[test]
fn control_sandbox_makes_the_home_directory_unresolvable() {
    let td = tempfile::tempdir().unwrap();
    let out = sandboxed(td.path(), "/usr/bin/id", &["-un"]);
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(
        !name.is_empty() && name.chars().all(|c| c.is_ascii_digit()),
        "passwd lookup still resolves ({name:?}); the precondition does not hold"
    );
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "backlog 49d9bcae: open defect (P12), remove ignore when fixed"]
fn install_without_a_home_does_not_write_cwd_and_report_success() {
    let td = tempfile::tempdir().unwrap();
    let out = sandboxed(td.path(), env!("CARGO_BIN_EXE_gauge"), &["install"]);
    let wrote_cwd = td.path().join(".claude/settings.json").exists();
    assert!(
        !wrote_cwd && !out.status.success(),
        "home unresolvable, yet install exited {:?} and wrote ./.claude/settings.json={wrote_cwd}; \
         stdout: {} stderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
