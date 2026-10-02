#![cfg(target_os = "macos")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Repro for backlog a1cc21f2: `budgetguard install` resolves the settings file
//! as `dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))`, so when the home
//! directory cannot be resolved it writes `./.claude/settings.json` under the
//! CURRENT directory and still prints "Installed Stop hook" — the operator reads
//! "installed" while the real harness has no hook.
//!
//! Inducing `home_dir() == None` on macOS: `dirs` reads `$HOME`, then falls back
//! to `getpwuid_r`. The test unsets HOME and runs the binary under
//! `sandbox-exec` with a profile that denies the opendirectoryd libinfo lookup,
//! so the passwd lookup fails. The same profile denies every write under the
//! real home directory, so a mis-resolved path cannot touch the live settings.
//! The control proves the precondition (passwd lookup really fails in the
//! sandbox) so the RED below is not an artefact of a still-resolving home.

use std::path::Path;
use std::process::Command;

fn profile() -> String {
    let real_home = std::env::var("HOME").expect("HOME set for the test runner");
    format!(
        "(version 1)(allow default)\
         (deny mach-lookup (global-name \"com.apple.system.opendirectoryd.libinfo\"))\
         (deny file-write* (subpath \"{real_home}\"))"
    )
}

fn sandboxed(cwd: &Path, prog: &str, args: &[&str]) -> std::process::Output {
    assert!(
        Path::new("/usr/bin/sandbox-exec").exists(),
        "sandbox-exec missing: home_dir()==None cannot be induced (undetermined, not clean)"
    );
    Command::new("/usr/bin/sandbox-exec")
        .arg("-p")
        .arg(profile())
        .arg(prog)
        .args(args)
        .current_dir(cwd)
        .env_remove("HOME")
        .output()
        .expect("spawn sandbox-exec")
}

/// Precondition: inside the sandbox the passwd lookup for our uid fails
/// (`id -un` falls back to printing the numeric uid), i.e. no home is resolvable.
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

#[test]
#[ignore = "backlog a1cc21f2: open defect, remove ignore when fixed"]
fn install_without_a_home_does_not_write_cwd_and_report_success() {
    let td = tempfile::tempdir().unwrap();
    let out = sandboxed(td.path(), env!("CARGO_BIN_EXE_budgetguard"), &["install"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let wrote_cwd = td.path().join(".claude/settings.json").exists();
    assert!(
        !wrote_cwd && !out.status.success(),
        "home unresolvable, yet install exited {:?} and wrote ./.claude/settings.json={wrote_cwd}; \
         stdout: {stdout} stderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}
