//! DoD14 b/c/d (move A): compass's cross-session discovery store is ONE store
//! per project, shared by every linked worktree, and a store that cannot be
//! resolved or read is reported as UNDETERMINED — never as an empty store.
//!
//! Black-box: every assertion drives the real `compass` binary with `HOME`
//! pointed at a tempdir (so `~/.compass/<key>/discovery.jsonl` lands in a
//! throwaway directory, never the developer's real `~/.compass`), inside a
//! throwaway git repo with linked worktrees made by `git worktree add`.
//!
//! Contract items covered (see the move-A contract):
//! 1. SHARED: a row recorded from linked worktree W1 is listed from the main
//!    worktree R, sibling W2, W1 itself and a subdirectory of W1.
//! 2. UNDETERMINED ROOT: a worktree with a corrupt `.git` gitfile refuses
//!    `list`/`record` (non-zero, `undetermined` on stderr, no `[]`, no write).
//! 3. ANTI-VACUITY CONTROL: a never-written store lists as exactly `[]`.
//! 4. READ ERROR: an unreadable store is undetermined for `list` (4a); `gap`
//!    keeps its candidate and says `discovery_dedup.status = undetermined`
//!    (4b), and `applied` when the store is readable; `route` keeps the
//!    opportunity refs and says `dedup unknown` (4c).
//! 5. LEGACY: a row in W1's OLD worktree-keyed file is still listed from W1;
//!    that legacy file being unreadable is undetermined.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

const CHARTER_JSON: &str = r#"{"north_star":"ship it","definition_of_done":["tests pass"],"measuring_stick":"tests passing","current_gap":"gap","next_action":"act","parked":[]}"#;
const OPP_TITLE: &str = "shared opportunity under test";

struct Fixture {
    _tmp: tempfile::TempDir,
    base: PathBuf,
    /// Main (primary) worktree.
    r: PathBuf,
    w1: PathBuf,
    w2: PathBuf,
    /// The sandboxed HOME the commands under test see.
    home: PathBuf,
}

fn git(cwd: &Path, args: &[&str]) {
    let out = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "SETUP: git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run_with(home: &Path, cwd: &Path, session: &str, args: &[&str], stdin: Option<&str>) -> Out {
    use std::io::Write;
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_compass"));
    harness_core::git_env::scrub(&mut cmd);
    cmd.args(args)
        .env("HOME", home)
        .env("CLAUDE_CODE_SESSION_ID", session)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env_remove("HARNESS_SANDBOX_HOME")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    let mut child = cmd.spawn().expect("compass spawns");
    if let Some(input) = stdin {
        child
            .stdin
            .take()
            .expect("stdin piped")
            .write_all(input.as_bytes())
            .expect("write stdin");
    }
    let out = child.wait_with_output().expect("compass runs");
    Out {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn run(home: &Path, cwd: &Path, session: &str, args: &[&str]) -> Out {
    run_with(home, cwd, session, args, None)
}

/// R (primary, one commit holding a charter), W1 and W2 linked worktrees.
fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let r = base.join("repo");
    let home = base.join("home");
    std::fs::create_dir_all(&r).unwrap();
    std::fs::create_dir_all(&home).unwrap();

    // Seed the charter in a non-git scratch dir with its OWN home, so setup
    // never writes into the sandboxed HOME under test.
    let scratch = base.join("scratch");
    let scratch_home = base.join("scratch-home");
    std::fs::create_dir_all(&scratch).unwrap();
    std::fs::create_dir_all(&scratch_home).unwrap();
    let o = run(
        &scratch_home,
        &scratch,
        "setup",
        &["charter", "--write", CHARTER_JSON],
    );
    assert_eq!(o.code, 0, "SETUP: charter seed failed: {}", o.stderr);

    git(&r, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(r.join(".compass")).unwrap();
    std::fs::copy(
        scratch.join(".compass/charter.md"),
        r.join(".compass/charter.md"),
    )
    .unwrap();
    std::fs::write(
        r.join(".gitignore"),
        ".compass/opportunities.json\n.compass/outcomes.json\n.compass/carve-state.json\n.claude/\n",
    )
    .unwrap();
    git(&r, &["add", "-A"]);
    git(&r, &["commit", "-q", "-m", "init"]);
    let w1 = base.join("w1");
    let w2 = base.join("w2");
    git(
        &r,
        &["worktree", "add", "-q", "-b", "f1", w1.to_str().unwrap()],
    );
    git(
        &r,
        &["worktree", "add", "-q", "-b", "f2", w2.to_str().unwrap()],
    );
    Fixture {
        _tmp: tmp,
        base,
        r,
        w1,
        w2,
        home,
    }
}

/// Put one opportunity (titled [`OPP_TITLE`], under the charter north_star)
/// into `<root>/.compass/opportunities.json` for each root. Created via the
/// real `compass opportunity add` in a non-git scratch dir with a separate
/// home (so no discovery row for it reaches the HOME under test), then copied.
fn seed_opportunity(fx: &Fixture, roots: &[&Path]) {
    let scratch = fx.base.join("opp-scratch");
    let scratch_home = fx.base.join("opp-scratch-home");
    std::fs::create_dir_all(&scratch).unwrap();
    std::fs::create_dir_all(&scratch_home).unwrap();
    let o = run(
        &scratch_home,
        &scratch,
        "setup",
        &["charter", "--write", CHARTER_JSON],
    );
    assert_eq!(o.code, 0, "SETUP: opp-scratch charter failed: {}", o.stderr);
    let o = run(
        &scratch_home,
        &scratch,
        "setup",
        &["opportunity", "add", "--title", OPP_TITLE],
    );
    assert_eq!(o.code, 0, "SETUP: opportunity add failed: {}", o.stderr);
    for root in roots {
        std::fs::create_dir_all(root.join(".compass")).unwrap();
        std::fs::copy(
            scratch.join(".compass/opportunities.json"),
            root.join(".compass/opportunities.json"),
        )
        .unwrap();
    }
}

/// Every `$HOME/.compass/*/discovery.jsonl` currently present (file OR dir).
fn store_files(home: &Path) -> Vec<PathBuf> {
    let dir = home.join(".compass");
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path().join("discovery.jsonl"))
        .filter(|p| p.exists())
        .collect();
    out.sort();
    out
}

/// Make a store file unreadable by replacing it with a DIRECTORY of the same
/// name (works even when the tests run as root, unlike `chmod 000`).
fn make_unreadable(path: &Path) {
    std::fs::remove_file(path).expect("SETUP: remove store file");
    std::fs::create_dir(path).expect("SETUP: create dir in place of store");
    std::fs::write(path.join("blocker"), "x").unwrap();
}

/// Record `title` from `cwd` and assert it exited 0.
fn record_ok(fx: &Fixture, cwd: &Path, session: &str, title: &str) {
    let o = run(
        &fx.home,
        cwd,
        session,
        &[
            "discovery",
            "record",
            "--title",
            title,
            "--session-id",
            session,
        ],
    );
    assert_eq!(
        o.code,
        0,
        "SETUP: discovery record from {} failed: {}",
        cwd.display(),
        o.stderr
    );
}

fn list_titles(fx: &Fixture, cwd: &Path) -> Vec<String> {
    let o = run(&fx.home, cwd, "lister", &["discovery", "list", "--json"]);
    assert_eq!(
        o.code,
        0,
        "discovery list --json from {} must exit 0, stderr: {}",
        cwd.display(),
        o.stderr
    );
    let v: Value = serde_json::from_str(o.stdout.trim()).unwrap_or_else(|e| {
        panic!(
            "list --json from {} not JSON ({e}): {}",
            cwd.display(),
            o.stdout
        )
    });
    v.as_array()
        .expect("list --json prints an array")
        .iter()
        .filter_map(|r| r["title"].as_str().map(str::to_string))
        .collect()
}

fn is_empty_json_array(s: &str) -> bool {
    matches!(serde_json::from_str::<Value>(s.trim()), Ok(Value::Array(a)) if a.is_empty())
}

fn assert_undetermined(o: &Out, what: &str) {
    assert_ne!(
        o.code, 0,
        "{what}: must exit NON-ZERO when the store is undetermined; stdout: {} stderr: {}",
        o.stdout, o.stderr
    );
    assert!(
        !is_empty_json_array(&o.stdout),
        "{what}: must NOT print an empty JSON array for an undetermined store; stdout: {}",
        o.stdout
    );
    assert!(
        o.stderr.to_lowercase().contains("undetermined"),
        "{what}: stderr must say `undetermined`; stderr: {}",
        o.stderr
    );
}

fn gap_json(fx: &Fixture, cwd: &Path, session: &str) -> Value {
    let o = run(&fx.home, cwd, session, &["gap"]);
    assert_eq!(
        o.code, 0,
        "gap must exit 0 (turn not broken); stderr: {}",
        o.stderr
    );
    serde_json::from_str(&o.stdout).unwrap_or_else(|e| panic!("gap not JSON ({e}): {}", o.stdout))
}

fn gap_has_opportunity(v: &Value) -> bool {
    v["opportunities"]
        .as_array()
        .expect("gap.opportunities is an array")
        .iter()
        .any(|o| o["title"] == OPP_TITLE)
}

// ---------------------------------------------------------------- item 1

#[test]
fn record_from_linked_worktree_is_listed_from_main_sibling_self_and_subdir() {
    let fx = fixture();
    let sub = fx.w1.join("sub").join("deeper");
    std::fs::create_dir_all(&sub).unwrap();
    record_ok(&fx, &fx.w1, "s1", "T shared across worktrees");

    for (label, cwd) in [
        ("main worktree R", &fx.r),
        ("sibling worktree W2", &fx.w2),
        ("recording worktree W1", &fx.w1),
        ("subdirectory of W1", &sub),
    ] {
        let titles = list_titles(&fx, cwd);
        assert!(
            titles.iter().any(|t| t == "T shared across worktrees"),
            "a row recorded from W1 must be listed from {label} ({}); got {titles:?}",
            cwd.display()
        );
    }
}

// ---------------------------------------------------------------- item 2

#[test]
fn corrupt_gitfile_worktree_list_is_undetermined_not_empty() {
    let fx = fixture();
    std::fs::write(fx.w2.join(".git"), "garbage\n").unwrap();
    let o = run(&fx.home, &fx.w2, "s1", &["discovery", "list", "--json"]);
    assert_undetermined(&o, "list --json in a worktree with a corrupt .git gitfile");
}

#[test]
fn corrupt_gitfile_worktree_record_is_refused_and_writes_no_store() {
    let fx = fixture();
    std::fs::write(fx.w2.join(".git"), "garbage\n").unwrap();
    let o = run(
        &fx.home,
        &fx.w2,
        "s1",
        &["discovery", "record", "--title", "T", "--session-id", "s1"],
    );
    assert_undetermined(&o, "record in a worktree with a corrupt .git gitfile");
    let written = store_files(&fx.home);
    assert!(
        written.is_empty(),
        "record with an undetermined root must not write to a guessed store; found {written:?}"
    );
}

// ---------------------------------------------------------------- item 3

#[test]
fn control_never_written_store_lists_exactly_empty_array_from_main_and_linked() {
    let fx = fixture();
    for (label, cwd) in [("R", &fx.r), ("W1", &fx.w1)] {
        let o = run(&fx.home, cwd, "s1", &["discovery", "list", "--json"]);
        assert_eq!(
            o.code, 0,
            "control: list from {label} with no store must exit 0; stderr: {}",
            o.stderr
        );
        assert!(
            is_empty_json_array(&o.stdout),
            "control: list from {label} with no store must print exactly []; got {}",
            o.stdout
        );
    }
}

// ---------------------------------------------------------------- item 4

/// Record from R, check it is visible, then make the single store unreadable.
fn record_then_break_store(fx: &Fixture) -> PathBuf {
    record_ok(fx, &fx.r, "other-session", "T recorded before the break");
    assert!(
        list_titles(fx, &fx.r)
            .iter()
            .any(|t| t == "T recorded before the break"),
        "SETUP: the record must be readable before the store is broken"
    );
    let files = store_files(&fx.home);
    assert_eq!(
        files.len(),
        1,
        "SETUP: expected exactly one discovery store, got {files:?}"
    );
    make_unreadable(&files[0]);
    files[0].clone()
}

#[test]
fn unreadable_store_list_is_undetermined_not_empty() {
    let fx = fixture();
    record_then_break_store(&fx);
    let o = run(&fx.home, &fx.r, "s1", &["discovery", "list", "--json"]);
    assert_undetermined(&o, "list --json with an unreadable store");
}

#[test]
fn gap_with_unreadable_store_keeps_candidate_and_reports_dedup_undetermined() {
    let fx = fixture();
    seed_opportunity(&fx, &[&fx.r]);
    record_then_break_store(&fx);
    let v = gap_json(&fx, &fx.r, "s1");
    assert!(
        gap_has_opportunity(&v),
        "gap must keep the opportunity when dedup cannot read the store; got {}",
        v["opportunities"]
    );
    let dd = &v["discovery_dedup"];
    assert!(
        dd.is_object(),
        "gap must carry a top-level `discovery_dedup` object; got {dd}"
    );
    assert_eq!(
        dd["status"], "undetermined",
        "unreadable store => discovery_dedup.status must be \"undetermined\"; got {dd}"
    );
    assert!(
        dd["reason"].as_str().is_some_and(|s| !s.trim().is_empty()),
        "undetermined discovery_dedup must carry a non-empty string reason; got {dd}"
    );
}

#[test]
fn gap_with_readable_store_reports_dedup_applied() {
    let fx = fixture();
    seed_opportunity(&fx, &[&fx.r]);
    record_ok(&fx, &fx.r, "other-session", "unrelated discovered row");
    let v = gap_json(&fx, &fx.r, "s1");
    assert!(
        gap_has_opportunity(&v),
        "gap must surface the opportunity nobody else owns; got {}",
        v["opportunities"]
    );
    assert_eq!(
        v["discovery_dedup"],
        serde_json::json!({ "status": "applied" }),
        "readable store => discovery_dedup must be exactly {{\"status\":\"applied\"}}"
    );
}

#[test]
fn route_with_unreadable_store_keeps_opportunity_refs_and_says_dedup_unknown() {
    // `compass route` refuses the primary checkout (it writes .claude/progress.md),
    // so it runs from linked worktree W1, which must resolve to the SAME shared
    // store that R recorded into.
    let fx = fixture();
    seed_opportunity(&fx, &[&fx.r, &fx.w1]);
    record_then_break_store(&fx);
    let dec = r#"{"goal":"g","tasks":[{"id":"t1","title":"do it","size":"m"}]}"#;
    let o = run_with(&fx.home, &fx.w1, "s1", &["route"], Some(dec));
    assert_eq!(
        o.code, 0,
        "route must exit 0 (turn not broken); stderr: {}",
        o.stderr
    );
    assert!(
        o.stdout.contains(OPP_TITLE),
        "route must keep the opportunity ref when dedup cannot read the store; stdout: {}",
        o.stdout
    );
    let both = format!("{}\n{}", o.stdout, o.stderr).to_lowercase();
    assert!(
        both.contains("dedup unknown"),
        "route must print `dedup unknown` for an unreadable store; stdout: {} stderr: {}",
        o.stdout,
        o.stderr
    );
}

// ---------------------------------------------------------------- item 5

/// The OLD (HEAD) worktree-keyed store path for linked worktree `wt`.
fn legacy_store(fx: &Fixture, wt: &Path) -> PathBuf {
    let top = std::fs::canonicalize(wt).unwrap();
    fx.home
        .join(".compass")
        .join(harness_core::store::project_key(&top))
        .join("discovery.jsonl")
}

fn write_legacy_row(path: &Path, title: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let row = serde_json::json!({
        "fingerprint": harness_core::discovery::fingerprint(title),
        "session_id": "legacy-session",
        "status": "discovered",
        "created_at": 1_700_000_000u64,
        "title": title,
    });
    std::fs::write(path, format!("{row}\n")).unwrap();
}

#[test]
fn legacy_worktree_keyed_record_is_still_listed_from_that_worktree() {
    let fx = fixture();
    let legacy = legacy_store(&fx, &fx.w1);
    write_legacy_row(&legacy, "T legacy row");
    let titles = list_titles(&fx, &fx.w1);
    assert!(
        titles.iter().any(|t| t == "T legacy row"),
        "a row in W1's legacy worktree-keyed store must still be listed from W1; got {titles:?}"
    );
}

#[test]
fn unreadable_legacy_worktree_keyed_store_is_undetermined() {
    let fx = fixture();
    let legacy = legacy_store(&fx, &fx.w1);
    write_legacy_row(&legacy, "T legacy row");
    make_unreadable(&legacy);
    let o = run(&fx.home, &fx.w1, "s1", &["discovery", "list", "--json"]);
    assert_undetermined(&o, "list --json from W1 with an unreadable legacy store");
}
