//! User ruling 2026-10-24 (DoD14(b) split, related cancelled backlog b0e1e4a2):
//! compass's outcome history is ONE store per project, shared by every linked
//! worktree. `.compass/outcomes.json` is gitignored, so resolving it off the cwd
//! gave each worktree its own private (and usually empty) history: an outcome
//! recorded from a worktree started over at seq 0, and `pivot-check` from that
//! worktree read only its own 1-entry store.
//!
//! The store is resolved to the project's MAIN worktree root
//! (`harness_core::projkey::main_worktree_root`). A root that cannot be
//! determined is refused (non-zero exit), never answered from a per-worktree
//! fallback store.
//!
//! Assertions are on the store FILES and the printed seq/streak, not only exit
//! codes.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const CHARTER_JSON: &str = r#"{"north_star":"ship it","definition_of_done":["tests pass"],"measuring_stick":"tests passing","current_gap":"gap","next_action":"act","parked":[]}"#;

struct Fixture {
    _tmp: tempfile::TempDir,
    primary: PathBuf,
    linked: PathBuf,
    home: PathBuf,
}

fn git(cwd: &Path, args: &[&str]) {
    let out = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn run(home: &Path, cwd: &Path, args: &[&str]) -> (i32, String, String) {
    let out = harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_compass")))
        .args(args)
        .env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
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

fn store(root: &Path) -> PathBuf {
    root.join(".compass").join("outcomes.json")
}

/// A primary checkout with a committed charter and an ignored outcomes store,
/// plus one linked worktree cut from it.
fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let primary = base.join("primary");
    let home = base.join("home");
    std::fs::create_dir_all(&primary).unwrap();
    std::fs::create_dir_all(&home).unwrap();

    // Seed the charter in a non-git scratch dir and copy it in, so setup never
    // writes through the boundary under test.
    let scratch = base.join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    let (code, _, err) = run(&home, &scratch, &["charter", "--write", CHARTER_JSON]);
    assert_eq!(code, 0, "fixture charter seed failed: {err}");

    git(&primary, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(primary.join(".compass")).unwrap();
    std::fs::copy(
        scratch.join(".compass/charter.md"),
        primary.join(".compass/charter.md"),
    )
    .unwrap();
    std::fs::write(
        primary.join(".gitignore"),
        ".compass/outcomes.json\n.compass/carve-state.json\n",
    )
    .unwrap();
    git(&primary, &["add", "-A"]);
    git(&primary, &["commit", "-q", "-m", "init"]);
    let linked = base.join("linked");
    git(
        &primary,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature",
            linked.to_str().unwrap(),
        ],
    );
    Fixture {
        _tmp: tmp,
        primary,
        linked,
        home,
    }
}

/// Write `n` backward outcomes (seq 0..n) straight into the primary checkout's
/// store — the history a main checkout already holds.
fn seed_primary_history(fx: &Fixture, n: u64) {
    let outcomes: Vec<serde_json::Value> = (0..n)
        .map(|i| {
            serde_json::json!({
                "seq": i,
                "recorded_at": 1_700_000_000u64 + i,
                "verdict": "backward",
                "evidence": [format!("measured regression {i}")],
                "north_star": "ship it",
                "current_gap": "gap",
            })
        })
        .collect();
    let body = serde_json::json!({ "outcomes": outcomes });
    std::fs::write(
        store(&fx.primary),
        serde_json::to_string_pretty(&body).unwrap(),
    )
    .unwrap();
}

fn seqs(path: &Path) -> Vec<u64> {
    let text = std::fs::read_to_string(path).expect("store readable");
    let v: serde_json::Value = serde_json::from_str(&text).expect("store parses");
    v["outcomes"]
        .as_array()
        .expect("outcomes array")
        .iter()
        .map(|o| o["seq"].as_u64().expect("seq"))
        .collect()
}

#[test]
fn outcome_from_linked_worktree_continues_the_main_checkout_history() {
    let fx = fixture();
    seed_primary_history(&fx, 3);

    let (code, out, err) = run(
        &fx.home,
        &fx.linked,
        &[
            "outcome",
            "--verdict",
            "backward",
            "--evidence",
            "ran tests",
        ],
    );
    assert_eq!(
        code, 0,
        "outcome from a linked worktree must succeed; out={out:?} err={err:?}"
    );
    assert!(
        out.contains("recorded outcome #3"),
        "seq must continue the main checkout's history (0,1,2 -> 3); out={out:?}"
    );
    assert_eq!(
        seqs(&store(&fx.primary)),
        vec![0, 1, 2, 3],
        "the outcome must land in the store the main checkout reads"
    );
    assert!(
        !store(&fx.linked).exists(),
        "no private per-worktree store may be created"
    );

    // pivot-check from either cwd sees the full 4-long backward streak.
    for cwd in [&fx.primary, &fx.linked] {
        let (code, out, err) = run(&fx.home, cwd, &["pivot-check"]);
        assert_eq!(code, 0, "pivot-check from {cwd:?}; out={out:?} err={err:?}");
        assert!(
            out.contains(r#""streak":4"#),
            "pivot-check from {cwd:?} must see the full history; out={out:?}"
        );
    }
}

/// Anti-vacuity control: a project whose store was never written reads as a
/// KNOWN empty history (exit 0, streak 0, persevere) from a linked worktree —
/// not as undetermined — and reading creates nothing.
#[test]
fn never_written_store_reads_as_known_empty_from_a_linked_worktree() {
    let fx = fixture();
    let (code, out, err) = run(&fx.home, &fx.linked, &["pivot-check"]);
    assert_eq!(code, 0, "out={out:?} err={err:?}");
    assert!(out.contains(r#""streak":0"#), "{out:?}");
    assert!(out.contains(r#""recommendation":"persevere""#), "{out:?}");
    assert!(out.contains("no outcomes recorded yet"), "{out:?}");
    assert!(!store(&fx.primary).exists());
    assert!(!store(&fx.linked).exists());
}

/// An undeterminable main worktree root is refused for both the write and the
/// read; neither falls back to a per-worktree store.
#[test]
fn undeterminable_main_root_is_refused_not_answered_per_worktree() {
    let fx = fixture();
    seed_primary_history(&fx, 3);
    // A `.git` FILE with no `gitdir:` line: whose worktree this is is unknown.
    std::fs::write(fx.linked.join(".git"), "not a gitdir pointer\n").unwrap();

    let (code, out, err) = run(&fx.home, &fx.linked, &["pivot-check"]);
    assert_ne!(code, 0, "pivot-check must refuse; out={out:?} err={err:?}");
    assert!(
        !out.contains("persevere"),
        "no confident answer; out={out:?}"
    );

    let (code, out, err) = run(
        &fx.home,
        &fx.linked,
        &["outcome", "--verdict", "forward", "--evidence", "ran tests"],
    );
    assert_ne!(code, 0, "outcome must refuse; out={out:?} err={err:?}");
    assert!(
        !store(&fx.linked).exists(),
        "no per-worktree fallback store"
    );
    assert_eq!(seqs(&store(&fx.primary)), vec![0, 1, 2]);
}

/// A corrupt shared store is not an empty history: pivot-check refuses.
#[test]
fn corrupt_shared_store_is_refused_by_pivot_check() {
    let fx = fixture();
    std::fs::write(store(&fx.primary), "{ not json").unwrap();
    let (code, out, err) = run(&fx.home, &fx.linked, &["pivot-check"]);
    assert_ne!(code, 0, "out={out:?} err={err:?}");
    assert!(!out.contains("persevere"), "{out:?}");
}
