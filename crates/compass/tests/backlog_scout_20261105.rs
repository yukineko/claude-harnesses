#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repros (scout 2026-11-05) for compass store defects. Each test is
//! `#[ignore]`d while its backlog item is open; run with `-- --ignored`.
//!
//! 1. `carve::save` (src/carve.rs) writes `.compass/carve-state.json` through a
//!    fixed `carve-state.json.tmp` with no lock: concurrent `compass apply`
//!    processes lose each other's recorded answers / trample the tmp file.
//! 2. `outcome::record` assigns `seq = last().seq + 1`; a store whose last entry
//!    has a lower seq than an earlier one yields a duplicate seq.
//! 3. `opportunity::slug_id(title)` hashes the title only, so two bets with the
//!    same title under different outcomes get the same id.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

const CHARTER_JSON: &str = r#"{"north_star":"ship it","definition_of_done":["tests pass"],"measuring_stick":"tests passing","current_gap":"gap","next_action":"act","parked":[]}"#;
const PROCS: usize = 8;
const PER_PROC: usize = 3;

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

#[test]
#[ignore = "backlog: open defect (scout 2026-11-05) carve::save is unlocked + fixed tmp name"]
fn concurrent_carve_apply_loses_no_answers() {
    let fx = fixture();
    let go = fx.proj.parent().unwrap().join("go");
    let mut kids = Vec::new();
    for p in 0..PROCS {
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
        let mut c = Command::new("sh");
        c.current_dir(&fx.proj)
            .env("HOME", &fx.home)
            .env("GO", &go)
            .env("BIN", bin())
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        c.arg("-c").arg(&script).arg("sh").args(&flat);
        kids.push(c.spawn().unwrap());
    }
    std::thread::sleep(std::time::Duration::from_millis(300));
    std::fs::write(&go, b"go").unwrap();
    for mut k in kids {
        k.wait().unwrap();
    }

    let dir = fx.proj.join(".compass");
    let left: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".tmp"))
        .collect();
    assert!(left.is_empty(), "leftover tmp files: {left:?}");
    let text = std::fs::read_to_string(dir.join("carve-state.json")).expect("carve state exists");
    let v: Value = serde_json::from_str(&text).expect("carve state parses");
    let n = v["answers"].as_array().unwrap().len();
    assert_eq!(
        n,
        PROCS * PER_PROC,
        "lost carve answers: {n} of {} survived",
        PROCS * PER_PROC
    );
}

#[test]
#[ignore = "backlog: open defect (scout 2026-11-05) outcome seq = last+1 duplicates on non-monotonic store"]
fn outcome_seq_is_unique_when_last_entry_has_lower_seq() {
    let fx = fixture();
    let store = fx.proj.join(".compass/outcomes.json");
    let mk = |seq: u64| {
        serde_json::json!({"seq": seq, "recorded_at": 1, "verdict": "forward",
            "evidence": ["e"], "north_star": "ship it", "current_gap": "gap"})
    };
    let seed = serde_json::json!({"outcomes": [mk(0), mk(3), mk(1), mk(2)]});
    std::fs::write(&store, serde_json::to_vec(&seed).unwrap()).unwrap();

    let out = cmd(&fx.home, &fx.proj)
        .args(["outcome", "--verdict", "forward", "--evidence", "new"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "record failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&store).unwrap()).unwrap();
    let seqs: Vec<u64> = v["outcomes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["seq"].as_u64().unwrap())
        .collect();
    let mut sorted = seqs.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        seqs.len(),
        "duplicate seq after record: {seqs:?}"
    );
}

#[test]
#[ignore = "backlog: open defect (scout 2026-11-05) opportunity id derived from title only"]
fn same_title_under_different_outcomes_gets_distinct_ids_or_is_refused() {
    let fx = fixture();
    let add = |outcome: &str| {
        cmd(&fx.home, &fx.proj)
            .args([
                "opportunity",
                "add",
                "--title",
                "speed up CI",
                "--outcome",
                outcome,
            ])
            .output()
            .unwrap()
    };
    let a = add("outcome-A");
    let b = add("outcome-B");
    assert!(a.status.success(), "first add failed");
    if !b.status.success() {
        return; // refusing the second record is an accepted resolution
    }
    let store = fx.proj.join(".compass/opportunities.json");
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&store).unwrap()).unwrap();
    let ids: Vec<&str> = v["opportunities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), 2, "both recorded: {ids:?}");
    assert_ne!(ids[0], ids[1], "two distinct bets share one id: {ids:?}");
}
