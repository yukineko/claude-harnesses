#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Scout 2026-11-07 findings for compass stores.
//!
//! 1. `evaluate_command` (src/main.rs) does load, overwrite and `carve::save`
//!    of the carve state WITHOUT holding `carve::lock`, so a concurrent
//!    `compass apply` loses answers (RED, `#[ignore]`d while the item is open).
//! 2. `outcome::record` fails on seq overflow (`checked_add`); PINNING test,
//!    green at HEAD (not a defect).
//!
//! F2 (`carve::reset` without the lock) is not filed: the reset-vs-apply
//! interleaving could not be made deterministic from outside the process.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

const CHARTER_JSON: &str = r#"{"north_star":"ship it","definition_of_done":["tests pass"],"measuring_stick":"tests passing","current_gap":"gap","next_action":"act","parked":[]}"#;
const APPLIERS: usize = 4;
const EVALUATORS: usize = 4;
const PER_PROC: usize = 8;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_compass"))
}

fn cmd(home: &Path, proj: &Path) -> Command {
    let mut c = Command::new(bin());
    harness_core::git_env::scrub(&mut c);
    c.current_dir(proj).env("HOME", home).stdin(Stdio::null());
    c
}

struct Fx {
    _tmp: tempfile::TempDir,
    proj: PathBuf,
    home: PathBuf,
}

fn fixture() -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let proj = base.join("proj");
    let home = base.join("home");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let out = cmd(&home, &proj)
        .args(["charter", "--write", CHARTER_JSON])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "charter seed failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Fx {
        _tmp: tmp,
        proj,
        home,
    }
}

fn sh(fx: &Fx, go: &Path, script: &str, args: &[String]) -> std::process::Child {
    let mut c = Command::new("sh");
    c.current_dir(&fx.proj)
        .env("HOME", &fx.home)
        .env("GO", go)
        .env("BIN", bin())
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    c.arg("-c").arg(script).arg("sh").args(args);
    c.spawn().unwrap()
}

#[test]
fn concurrent_evaluate_and_apply_lose_no_answers() {
    let fx = fixture();
    let go = fx.proj.parent().unwrap().join("go");
    let mut kids = Vec::new();
    for p in 0..APPLIERS {
        let mut script = String::from("while [ ! -e \"$GO\" ]; do :; done\n");
        let mut flat: Vec<String> = Vec::new();
        for i in 0..PER_PROC {
            flat.push(format!(
                r#"{{"gate":"g","reference":"r-{p}-{i}","value":"v-{p}-{i}","defer":false}}"#
            ));
            script.push_str(&format!(
                "\"$BIN\" apply --answer \"${{{}}}\"\n",
                flat.len()
            ));
        }
        kids.push(sh(&fx, &go, &script, &flat));
    }
    for _ in 0..EVALUATORS {
        let mut script = String::from("while [ ! -e \"$GO\" ]; do :; done\n");
        for _ in 0..PER_PROC {
            script.push_str("\"$BIN\" evaluate\n");
        }
        kids.push(sh(&fx, &go, &script, &[]));
    }
    std::thread::sleep(std::time::Duration::from_millis(300));
    std::fs::write(&go, b"go").unwrap();
    for mut k in kids {
        k.wait().unwrap();
    }

    let text = std::fs::read_to_string(fx.proj.join(".compass/carve-state.json"))
        .expect("carve state exists");
    let v: Value = serde_json::from_str(&text).expect("carve state parses");
    let n = v["answers"].as_array().unwrap().len();
    assert_eq!(
        n,
        APPLIERS * PER_PROC,
        "lost carve answers to unlocked evaluate: {n} of {} survived",
        APPLIERS * PER_PROC
    );
}

#[test]
fn outcome_record_refuses_seq_overflow_and_leaves_store_unchanged() {
    let fx = fixture();
    let store = fx.proj.join(".compass/outcomes.json");
    let seed = serde_json::json!({"outcomes": [{"seq": u64::MAX, "recorded_at": 1,
        "verdict": "forward", "evidence": ["e"], "north_star": "ship it",
        "current_gap": "gap"}]});
    let bytes = serde_json::to_vec(&seed).unwrap();
    std::fs::write(&store, &bytes).unwrap();

    let out = cmd(&fx.home, &fx.proj)
        .args(["outcome", "--verdict", "forward", "--evidence", "new"])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "record must fail on seq overflow; stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(
        std::fs::read(&store).unwrap(),
        bytes,
        "store must be unchanged after a refused record"
    );
}
