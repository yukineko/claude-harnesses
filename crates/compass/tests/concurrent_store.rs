#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (scout 2026-11-05): the outcome and opportunity stores do an unlocked
//! load -> push -> save with a fixed `json.tmp` sibling name. Concurrent
//! `compass` processes lose records (last rename wins) and trample each other's
//! tmp file.
//!
//! Required of the fix (done_criteria):
//!   * N concurrent recorders => all N records present afterwards;
//!   * no leftover `*.tmp` in the store dir;
//!   * NAMING CONSTRAINT: the exclusive lock is the advisory lockfile
//!     `<store file>.lock` (same convention as `harness_core::lessons::acquire_lock`:
//!     the lock is HELD iff that file exists). If it is held and cannot be
//!     acquired, compass exits non-zero and the store is byte-identical.
//!
//! Processes start behind a barrier (a `go` file the wrapper shells spin on) and
//! each shell records several entries back-to-back, so the race window is hit
//! on every run, not only by luck.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

const CHARTER_JSON: &str = r#"{"north_star":"ship it","definition_of_done":["tests pass"],"measuring_stick":"tests passing","current_gap":"gap","next_action":"act","parked":[]}"#;
const PROCS: usize = 12;
const PER_PROC: usize = 6;

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

/// Run PROCS shells at once; shell `p` runs PER_PROC compass invocations whose
/// argv is `args(p, i)`. Returns when all have exited.
fn race(fx: &Fx, args: impl Fn(usize, usize) -> Vec<String>) {
    let go = fx.proj.parent().unwrap().join("go");
    let mut kids = Vec::new();
    for p in 0..PROCS {
        // The script only references positional parameters; every compass
        // argument is passed as data after `sh -c <script> sh`.
        let mut script = String::from("while [ ! -e \"$GO\" ]; do :; done\n");
        let mut flat: Vec<String> = Vec::new();
        for i in 0..PER_PROC {
            script.push_str("\"$BIN\"");
            for s in args(p, i) {
                flat.push(s);
                script.push_str(&format!(" \"${{{}}}\"", flat.len()));
            }
            script.push('\n');
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
}

fn read_json(p: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(p).expect("store exists")).expect("store parses")
}

fn no_tmp_left(dir: &Path) {
    let left: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".tmp"))
        .collect();
    assert!(left.is_empty(), "leftover tmp files in {dir:?}: {left:?}");
}

#[test]
fn concurrent_outcome_record_loses_no_entries() {
    let fx = fixture();
    race(&fx, |p, i| {
        vec![
            "outcome".into(),
            "--verdict".into(),
            "forward".into(),
            "--evidence".into(),
            format!("ev-{p}-{i}"),
        ]
    });
    let store = fx.proj.join(".compass/outcomes.json");
    let v = read_json(&store);
    let n = v["outcomes"].as_array().unwrap().len();
    assert_eq!(
        n,
        PROCS * PER_PROC,
        "lost outcome records: {n} of {} survived",
        PROCS * PER_PROC
    );
    no_tmp_left(store.parent().unwrap());
}

#[test]
fn concurrent_opportunity_add_loses_no_entries() {
    let fx = fixture();
    race(&fx, |p, i| {
        vec![
            "opportunity".into(),
            "add".into(),
            "--title".into(),
            format!("bet-{p}-{i}"),
        ]
    });
    let store = fx.proj.join(".compass/opportunities.json");
    let v = read_json(&store);
    let n = v["opportunities"].as_array().unwrap().len();
    assert_eq!(
        n,
        PROCS * PER_PROC,
        "lost opportunity records: {n} of {} survived",
        PROCS * PER_PROC
    );
    no_tmp_left(store.parent().unwrap());
}

/// A held `<store>.lock` must refuse the write: non-zero exit, store untouched.
fn lock_refusal(args: &[&str], store_rel: &str, seed: &str) {
    let fx = fixture();
    let store = fx.proj.join(store_rel);
    std::fs::write(&store, seed).unwrap();
    let before = std::fs::read(&store).unwrap();
    let mut lock = store.as_os_str().to_owned();
    lock.push(".lock");
    std::fs::write(&lock, b"held by test").unwrap();

    let out = cmd(&fx.home, &fx.proj).args(args).output().unwrap();
    assert!(
        !out.status.success(),
        "write must be refused while {lock:?} is held; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read(&store).unwrap(),
        before,
        "store changed despite refused lock"
    );
    no_tmp_left(store.parent().unwrap());
}

#[test]
fn outcome_refused_when_store_lock_held() {
    lock_refusal(
        &["outcome", "--verdict", "forward", "--evidence", "x"],
        ".compass/outcomes.json",
        r#"{"outcomes":[]}"#,
    );
}

#[test]
fn opportunity_refused_when_store_lock_held() {
    lock_refusal(
        &["opportunity", "add", "--title", "bet"],
        ".compass/opportunities.json",
        r#"{"opportunities":[]}"#,
    );
}
