#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Repro for backlog b127826e: with `mode = "subproces"` (typo) the Stop gate
//! now blocks `config-invalid` (3ca750b9) and its reason points the user at
//! `propguard status`, but `propguard status` prints `mode: inject` — the
//! built-in default that is NOT in effect — and never says the config is
//! invalid.
use std::path::PathBuf;
use std::process::Command;

#[test]
#[ignore = "backlog b127826e: open defect, remove ignore when fixed"]
fn status_names_an_invalid_mode_instead_of_showing_the_default() {
    let dir: PathBuf = std::env::temp_dir().join(format!("pg-b127826e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("propguard.toml"), "mode = \"subproces\"\n").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_propguard"))
        .arg("status")
        .current_dir(&dir)
        .env("HOME", &dir)
        .env("HARNESS_TRUST_ALL", "1")
        .env("PROPGUARD_STATE_DIR", dir.join("state"))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let _ = std::fs::remove_dir_all(&dir);
    let low = stdout.to_lowercase();
    let says_invalid = low.contains("invalid") || low.contains("config-invalid");
    assert!(
        says_invalid,
        "status must say the config is invalid; it printed:\n{stdout}\n(stderr: {stderr})"
    );
}
