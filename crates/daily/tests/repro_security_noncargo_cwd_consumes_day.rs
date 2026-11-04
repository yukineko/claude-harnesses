//! RED repro (2026-11-04): the built-in `security` task (`cargo deny check …`)
//! runs in whatever directory the FIRST session of the day starts in. If that
//! is not a Cargo project, cargo-deny fails with "doesn't contain a Cargo.toml
//! file", the failure still consumes the day's slot (`mark_done` runs
//! regardless of outcome), and no Cargo project is audited that day.
//!
//! Measured on this machine, ~/.daily/reports.jsonl 2026-07-09..2026-11-04:
//! 79 `security` runs, 62 `fail` — every one of them with
//! "the directory … doesn't contain a Cargo.toml file" (e.g. vpnproxy,
//! medipartner_transaction). The 17 `ok` rows do not record which directory
//! was audited, so whether a given repo was ever audited is unobservable.
//!
//! cargo is faked: `$CARGO_HOME/bin/cargo` appends its `$PWD` to a log, so the
//! test observes *where* the audit ran without touching the network.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(unix)]
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

fn session_start(home: &Path, cwd: &Path) -> (String, String) {
    let payload = format!(
        r#"{{"hook_event_name":"SessionStart","cwd":"{}"}}"#,
        cwd.display()
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_daily"))
        .arg("session-start")
        .env("HOME", home)
        .env("CARGO_HOME", home.join(".cargo"))
        .env("PATH", "/usr/bin:/bin")
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("daily spawns");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

struct Fixture {
    _tmp: tempfile::TempDir,
    home: std::path::PathBuf,
    non_cargo: std::path::PathBuf,
    cargo_proj: std::path::PathBuf,
    log: std::path::PathBuf,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let bin = home.join(".cargo").join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let log = tmp.path().join("cargo-ran-in.log");
    // Fake cargo: record where it ran; fail like real cargo-deny when there is
    // no Cargo.toml, succeed otherwise.
    let script = format!(
        "#!/bin/sh\npwd -P >> '{}'\n[ -f Cargo.toml ] || {{ echo \"[ERROR] the directory $(pwd) doesn't contain a Cargo.toml file\" >&2; exit 1; }}\necho 'advisories ok'\n",
        log.display()
    );
    let cargo = bin.join("cargo");
    std::fs::write(&cargo, script).unwrap();
    std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o755)).unwrap();

    let non_cargo = tmp.path().join("pyrepo");
    let cargo_proj = tmp.path().join("rustrepo");
    std::fs::create_dir_all(&non_cargo).unwrap();
    std::fs::create_dir_all(&cargo_proj).unwrap();
    std::fs::write(
        cargo_proj.join("Cargo.toml"),
        "[package]\nname=\"x\"\nversion=\"0.1.0\"\n",
    )
    .unwrap();
    Fixture {
        non_cargo: non_cargo.canonicalize().unwrap(),
        cargo_proj: cargo_proj.canonicalize().unwrap(),
        home,
        log,
        _tmp: tmp,
    }
}

/// A non-Cargo first session must not use up the day: a Cargo project opened
/// later the same day must still be audited.
#[test]
fn noncargo_first_session_does_not_starve_cargo_project_audit() {
    let f = fixture();
    let (o1, e1) = session_start(&f.home, &f.non_cargo);
    let (o2, e2) = session_start(&f.home, &f.cargo_proj);
    let ran_in = std::fs::read_to_string(&f.log).unwrap_or_default();
    assert!(
        ran_in
            .lines()
            .any(|l| Path::new(l.trim()) == f.cargo_proj.as_path()),
        "security audit never ran in the Cargo project opened later the same day.\n\
         cargo ran in: {ran_in:?}\n1st stdout={o1} stderr={e1}\n2nd stdout={o2} stderr={e2}"
    );
}

/// A non-Cargo directory is "not applicable", not an audit failure: it must
/// not be recorded as `fail` (that row is read as "the audit found a problem"
/// / "the audit ran today" and hides that nothing was audited).
#[test]
fn noncargo_session_is_not_reported_as_audit_fail() {
    let f = fixture();
    let _ = session_start(&f.home, &f.non_cargo);
    let report =
        std::fs::read_to_string(f.home.join(".daily").join("reports.jsonl")).unwrap_or_default();
    for line in report.lines() {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        if v["task"] == "security" {
            assert_ne!(
                v["status"], "fail",
                "non-Cargo cwd was recorded as a security-audit failure: {line}"
            );
        }
    }
}

/// Every security report row that claims the audit ran must say WHERE, so
/// "was repo X audited?" is observable from the report.
#[test]
fn security_report_records_audited_directory() {
    let f = fixture();
    let _ = session_start(&f.home, &f.cargo_proj);
    let report =
        std::fs::read_to_string(f.home.join(".daily").join("reports.jsonl")).unwrap_or_default();
    let row = report
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .find(|v| v["task"] == "security" && v["status"] == "ok")
        .unwrap_or_else(|| panic!("no ok security row in report: {report:?}"));
    let dir = row["dir"]
        .as_str()
        .unwrap_or_else(|| panic!("security ok row has no `dir` field: {row}"));
    assert_eq!(Path::new(dir), f.cargo_proj.as_path(), "row: {row}");
}
