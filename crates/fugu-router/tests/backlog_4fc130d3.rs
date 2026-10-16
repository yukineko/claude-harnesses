//! backlog 4fc130d3: `record --cost 0` exits 0 and stores `0.0`, but the store
//! reads a present `0.0` back as UNMEASURED. So a caller that supplied a
//! measured zero cost gets success while its measurement is silently turned
//! into "unmeasured". The property: a supplied `--cost 0` must either be
//! rejected at write (like `--duration 0`), or survive as a measured value.
//!
//! Drives the real binary against a sandboxed `$HOME`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn temp_home() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "fugu-router-4fc130d3-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create temp home");
    dir
}

fn run_in(home: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_fugu-router"))
        .args(args)
        .env("HOME", home)
        .output()
        .expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
#[ignore = "backlog 4fc130d3: RED repro"]
fn supplied_zero_cost_is_rejected_or_kept_as_measured() {
    let home = temp_home();
    let (code, out, err) = run_in(
        &home,
        &[
            "record", "--title", "t", "--model", "sonnet", "--status", "verified", "--cost", "0",
        ],
    );
    if code != 0 {
        // Rejected at write, like --duration 0: acceptable.
        return;
    }
    let _ = (out, err);
    let (code, out, err) = run_in(&home, &["stats", "--json"]);
    assert_eq!(code, 0, "stats failed: {err}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(v["episodes"], 1, "precondition: {v}");
    assert_eq!(
        v["cost_coverage"]["recorded"], 1,
        "`record --cost 0` exited 0, so the supplied cost must read back as measured, \
         not silently as unmeasured: {v}"
    );
}
