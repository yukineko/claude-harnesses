//! backlog e3ca0b92: `carve::load` (crates/compass/src/carve.rs) collapses a
//! read/parse error of `.compass/carve-state.json` into `None`, the same answer
//! as "no state file". `compass evaluate` then re-initializes and SAVES a fresh
//! state over the unreadable one, exit 0 — "could not read the carve" and "there
//! is no carve" become the same outcome, and the recorded answers in the
//! unparseable file are overwritten without a word (CLAUDE.md §3: parse failure
//! is Undetermined, never Absent).
//!
//! Property: with an unparseable state file, `evaluate` must not behave exactly
//! like the absent case — it must either fail (non-zero) or leave the file it
//! could not read untouched.

use std::path::Path;
use std::process::{Command, Stdio};

fn run(home: &Path, shim: &Path, cwd: &Path, args: &[&str]) -> (i32, String, String) {
    let path = format!(
        "{}:{}",
        shim.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::new(env!("CARGO_BIN_EXE_compass"))
        .args(args)
        .env("HOME", home)
        .env("PATH", path)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .expect("compass runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

struct Fx {
    _tmp: tempfile::TempDir,
    home: std::path::PathBuf,
    shim: std::path::PathBuf,
    cwd: std::path::PathBuf,
}

/// A scratch NON-git cwd (the store write guard is a no-op outside a repo).
fn fx() -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let (home, shim, cwd) = (base.join("home"), base.join("shim"), base.join("proj"));
    for d in [&home, &shim, &cwd] {
        std::fs::create_dir_all(d).unwrap();
    }
    let condukt = shim.join("condukt");
    std::fs::write(&condukt, "#!/bin/sh\nexit 1\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&condukt, std::fs::Permissions::from_mode(0o755)).unwrap();
    Fx {
        _tmp: tmp,
        home,
        shim,
        cwd,
    }
}

/// CONTROL: absent state -> evaluate succeeds and writes a fresh state.
#[test]
fn control_absent_state_is_initialized() {
    let f = fx();
    let (code, _out, err) = run(&f.home, &f.shim, &f.cwd, &["evaluate"]);
    assert_eq!(
        code, 0,
        "control: evaluate with no state must succeed: {err}"
    );
    assert!(f.cwd.join(".compass/carve-state.json").exists());
}

#[test]
#[ignore = "backlog e3ca0b92: RED repro"]
fn unparseable_state_is_not_treated_as_absent() {
    let f = fx();
    let state = f.cwd.join(".compass/carve-state.json");
    std::fs::create_dir_all(state.parent().unwrap()).unwrap();
    let corrupt = b"{\"round\": 3, \"answers\": [ TRUNCATED";
    std::fs::write(&state, corrupt).unwrap();

    let (code, out, err) = run(&f.home, &f.shim, &f.cwd, &["evaluate"]);
    let after = std::fs::read(&state).unwrap_or_default();
    assert!(
        code != 0 || after == corrupt,
        "an unparseable carve-state.json was treated exactly like an absent one: \
         evaluate exited {code} and replaced the file it could not read \
         (now {} bytes). stdout={out} stderr={err}",
        after.len()
    );
}
