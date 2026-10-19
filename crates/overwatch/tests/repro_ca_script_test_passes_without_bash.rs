#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Repro: `continuous_audit_script.rs::continuous_audit_script_behaves` returns
//! early -- i.e. PASSES -- when `bash` cannot be spawned
//! (`if Command::new("bash").arg("--version").output().is_err() { return; }`).
//! "Could not run the check" is reported as "the check passed".
//!
//! Method: run the already-built `continuous_audit_script` test binary as a
//! child with an EMPTY PATH (and an empty cwd, so no `bash` is findable) and
//! assert that the test does NOT pass. Today it reports `1 passed`.
//!
//! Run with both test targets so the sibling binary is built:
//!   cargo test -p overwatch --test continuous_audit_script \
//!     --test repro_ca_script_test_passes_without_bash -- --ignored
use std::path::PathBuf;
use std::process::Command;

fn sibling_test_binary() -> PathBuf {
    let dir = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let picked = pick_sibling(&dir);
    assert!(
        picked.is_some(),
        "UNDETERMINED: sibling test binary continuous_audit_script-* not built in {} \
         (run with --test continuous_audit_script as well)",
        dir.display()
    );
    picked.unwrap()
}

/// Choose the `continuous_audit_script-<hash>` test binary to run. `target/`
/// keeps every build that ever happened, so several can coexist.
fn pick_sibling(dir: &std::path::Path) -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_none()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("continuous_audit_script-"))
        })
        .collect();
    found.sort();
    found.pop()
}

/// Repro: with an older build whose hash sorts LAST next to the build cargo
/// just made, the picker must return the current (newest) build. Picking by
/// file name makes the repro measure arbitrary stale code: it passed at the
/// pre-fix rev 7146b301 and failed at the fixed HEAD (backlog 73e49761 could
/// not be closed on 2026-10-20).
#[test]
#[ignore = "repro: pick_sibling selects by file-name order, not the current build; remove ignore when fixed"]
fn pick_sibling_returns_the_newest_build_not_the_last_name() {
    let dir = std::env::temp_dir().join(format!("ow-repro-pick-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let stale = dir.join("continuous_audit_script-ffffffffffffffff");
    let current = dir.join("continuous_audit_script-0000000000000000");
    std::fs::write(&stale, b"stale").unwrap();
    std::fs::write(&current, b"current").unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(86_400);
    std::fs::File::options()
        .write(true)
        .open(&stale)
        .unwrap()
        .set_modified(old)
        .unwrap();
    let picked = pick_sibling(&dir);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        picked.as_deref(),
        Some(current.as_path()),
        "picked a stale build by name order instead of the newest build"
    );
}

#[test]
fn script_test_must_not_pass_when_bash_is_unavailable() {
    let bin = sibling_test_binary();
    let empty = std::env::temp_dir().join(format!("ow-repro-nobash-{}", std::process::id()));
    std::fs::create_dir_all(&empty).unwrap();

    // Control: with a normal PATH the same binary really runs and passes.
    let ok = Command::new(&bin)
        .args(["continuous_audit_script_behaves", "--exact"])
        .env("PATH", "/usr/bin:/bin")
        .current_dir(&empty)
        .output()
        .unwrap();
    assert!(
        ok.status.success() && String::from_utf8_lossy(&ok.stdout).contains("1 passed"),
        "control failed: {ok:?}"
    );

    let out = Command::new(&bin)
        .args(["continuous_audit_script_behaves", "--exact"])
        .env_clear()
        .env("PATH", "")
        .current_dir(&empty)
        .output()
        .unwrap();
    let so = String::from_utf8_lossy(&out.stdout);
    let se = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "continuous_audit_script_behaves PASSED with no bash available (it returned \
         early without checking anything).\nstdout={so}\nstderr={se}"
    );
}
