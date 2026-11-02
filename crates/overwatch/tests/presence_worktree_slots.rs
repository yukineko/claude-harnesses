// 統合テスト: テスト内の unwrap/expect/panic は意図的な assert（production 側は workspace の deny のまま）。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Independent tests for the per-worktree presence slots (overwatch 0.2.109):
//! SessionEnd scoping, legacy (pre-slot) entries, sequential multi-worktree
//! attribution, entry-level pruning, and the repo_root-undetermined case.
//!
//! Hooks are driven by running `overwatch presence` with the payload on
//! stdin; HOME is a tempdir sandbox. Every child has a deadline and is killed
//! if it overruns (no unbounded wait).

use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_overwatch");
const DEADLINE: Duration = Duration::from_secs(60);
const H25: i64 = 25 * 3600;

struct Sandbox {
    _t: TempDir,
    root: PathBuf,
    home: PathBuf,
    repo: PathBuf,
    wt: PathBuf,
    transcript: PathBuf,
}

fn git(dir: &Path, args: &[&str]) {
    let out = harness_core::git_env::scrub(&mut Command::new("git"))
        .current_dir(dir)
        .env("HOME", dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn sandbox() -> Sandbox {
    let t = TempDir::new().unwrap();
    let root = t.path().canonicalize().unwrap();
    let home = root.join("home");
    let repo = root.join("repo");
    let wt = root.join("wt-b");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    std::fs::write(repo.join("README"), "x\n").unwrap();
    git(&repo, &["add", "README"]);
    git(
        &repo,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            "init",
        ],
    );
    git(
        &repo,
        &["worktree", "add", "-q", "-b", "side", wt.to_str().unwrap()],
    );
    let wt = wt.canonicalize().unwrap();
    let transcript = root.join("transcripts").join("t.jsonl");
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    std::fs::write(&transcript, "{\"type\":\"user\"}\n").unwrap();
    Sandbox {
        _t: t,
        root,
        home,
        repo,
        wt,
        transcript,
    }
}

fn run_bounded(mut cmd: Command, stdin: Option<&str>) -> Output {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    let mut child = cmd.spawn().unwrap();
    if let Some(s) = stdin {
        let mut si = child.stdin.take().unwrap();
        si.write_all(s.as_bytes()).unwrap();
    }
    let start = Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if start.elapsed() > DEADLINE {
            let _ = child.kill();
            panic!("child exceeded {DEADLINE:?} deadline and was killed");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn ow(sb: &Sandbox, cwd: &Path) -> Command {
    let mut c = Command::new(BIN);
    harness_core::git_env::scrub(&mut c);
    c.current_dir(cwd)
        .env("HOME", &sb.home)
        .env("PATH", "/usr/bin:/bin")
        .env_remove("HARNESS_SANDBOX_HOME")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("OVERWATCH_RUN_ID");
    c
}

fn fire(sb: &Sandbox, payload: &Value) {
    let cwd = PathBuf::from(payload["cwd"].as_str().unwrap());
    let mut c = ow(sb, &cwd);
    c.arg("presence");
    let out = run_bounded(c, Some(&payload.to_string()));
    assert!(
        out.status.success(),
        "presence hook exited {:?}\nstderr:{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn ev(sb: &Sandbox, sid: &str, name: &str, cwd: &Path) -> Value {
    json!({"session_id":sid,"transcript_path":sb.transcript,"cwd":cwd,
           "hook_event_name":name})
}

fn start(sb: &Sandbox, sid: &str, cwd: &Path) {
    fire(sb, &ev(sb, sid, "SessionStart", cwd));
}

fn end(sb: &Sandbox, sid: &str, cwd: &Path) {
    fire(sb, &ev(sb, sid, "SessionEnd", cwd));
}

fn write(sb: &Sandbox, sid: &str, cwd: &Path, file: &Path) {
    let mut p = ev(sb, sid, "PostToolUse", cwd);
    p["tool_name"] = json!("Write");
    p["tool_input"] = json!({"file_path":file});
    fire(sb, &p);
}

fn status(sb: &Sandbox, json_flag: bool) -> (Value, String) {
    status_at(sb, &sb.repo, json_flag)
}

fn status_at(sb: &Sandbox, cwd: &Path, json_flag: bool) -> (Value, String) {
    let mut c = ow(sb, cwd);
    c.arg("status");
    if json_flag {
        c.arg("--json");
    }
    let out = run_bounded(c, None);
    let raw = String::from_utf8_lossy(&out.stdout).to_string();
    if !json_flag {
        return (Value::Null, raw);
    }
    let v: Value = serde_json::from_str(&raw).unwrap_or_else(|e| {
        panic!(
            "status --json not JSON ({e}):\n{raw}\nstderr:{}",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    (v, raw)
}

fn rows(sb: &Sandbox, sid: &str) -> Vec<Value> {
    rows_at(sb, &sb.repo, sid)
}

fn rows_at(sb: &Sandbox, cwd: &Path, sid: &str) -> Vec<Value> {
    let (v, raw) = status_at(sb, cwd, true);
    v["active_sessions"]
        .as_array()
        .unwrap_or_else(|| panic!("no active_sessions:\n{raw}"))
        .iter()
        .filter(|e| e["session_id"] == sid)
        .cloned()
        .collect()
}

fn row_for<'a>(rows: &'a [Value], worktree: &Path) -> &'a Value {
    rows.iter()
        .find(|r| {
            PathBuf::from(r["worktree"].as_str().unwrap_or(""))
                .canonicalize()
                .is_ok_and(|p| p == worktree)
        })
        .unwrap_or_else(|| panic!("no row for worktree {}: {rows:?}", worktree.display()))
}

fn paths(row: &Value) -> Vec<String> {
    row["written_paths"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap().to_string())
        .collect()
}

fn presence_json(sb: &Sandbox) -> PathBuf {
    let mut found = Vec::new();
    let mut stack = vec![sb.home.join(".overwatch")];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for ent in rd.flatten() {
            let p = ent.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.file_name().is_some_and(|n| n == "presence.json") {
                found.push(p);
            }
        }
    }
    assert_eq!(found.len(), 1, "expected one presence.json: {found:?}");
    found.pop().unwrap()
}

fn read_reg(sb: &Sandbox) -> Value {
    serde_json::from_str(&std::fs::read_to_string(presence_json(sb)).unwrap()).unwrap()
}

fn write_reg(sb: &Sandbox, reg: &Value) {
    std::fs::write(
        presence_json(sb),
        serde_json::to_string_pretty(reg).unwrap(),
    )
    .unwrap();
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// Seed presence.json with an unrelated session so the file exists, then
/// replace the registry with a hand-written legacy (pre-0.2.109) entry for
/// `sid`: top-level written_paths, no `worktrees` key.
fn seed_legacy(sb: &Sandbox, sid: &str, legacy_paths: &[&str]) {
    start(sb, "seed-other", &sb.repo);
    let entry = json!({
        "session_id": sid,
        "cwd": sb.repo,
        "worktree": sb.repo,
        "transcript_path": sb.transcript,
        "written_paths": legacy_paths,
        "last_seen": now_secs(),
        "ended": false
    });
    let mut reg = serde_json::Map::new();
    reg.insert(sid.to_string(), entry);
    write_reg(sb, &Value::Object(reg));
}

// ---- 1. SessionEnd scoping ------------------------------------------------

#[test]
fn session_end_from_one_worktree_leaves_other_row_and_entry_not_ended() {
    let sb = sandbox();
    let sid = "s-end-scope";
    start(&sb, sid, &sb.repo);
    start(&sb, sid, &sb.wt);
    end(&sb, sid, &sb.wt);

    let r = rows(&sb, sid);
    assert_eq!(r.len(), 2, "one row per worktree: {r:?}");
    assert_eq!(row_for(&r, &sb.wt)["liveness"], "ended", "{r:?}");
    assert_ne!(
        row_for(&r, &sb.repo)["liveness"],
        "ended",
        "wt-B SessionEnd must not end the repo row: {r:?}"
    );
    let reg = read_reg(&sb);
    assert_eq!(reg[sid]["ended"], false, "entry must stay open: {reg}");
}

#[test]
fn session_end_in_every_slot_ends_the_entry() {
    let sb = sandbox();
    let sid = "s-end-all";
    start(&sb, sid, &sb.repo);
    start(&sb, sid, &sb.wt);
    end(&sb, sid, &sb.wt);
    end(&sb, sid, &sb.repo);

    let reg = read_reg(&sb);
    assert_eq!(reg[sid]["ended"], true, "all slots ended: {reg}");
    let r = rows(&sb, sid);
    assert_eq!(r.len(), 2, "{r:?}");
    assert!(r.iter().all(|x| x["liveness"] == "ended"), "{r:?}");
}

// ---- 2./3. legacy entries -------------------------------------------------

#[test]
fn legacy_written_paths_listed_as_worktree_undetermined_in_json() {
    let sb = sandbox();
    let sid = "s-legacy";
    seed_legacy(&sb, sid, &["/r/legacy-a.rs", "/r/legacy-b.rs"]);

    let r = rows(&sb, sid);
    assert_eq!(r.len(), 1, "{r:?}");
    assert_eq!(r[0]["worktree"], "", "no guessed worktree: {r:?}");
    assert!(
        r[0]["worktree_undetermined"].is_string(),
        "reason must be set: {r:?}"
    );
    assert_eq!(paths(&r[0]), vec!["/r/legacy-a.rs", "/r/legacy-b.rs"]);
}

#[test]
fn legacy_written_paths_shown_as_unknown_worktree_in_text() {
    let sb = sandbox();
    let sid = "s-legacy-text";
    seed_legacy(&sb, sid, &["/r/legacy-text.rs"]);

    let (_, text) = status(&sb, false);
    let line = text
        .lines()
        .find(|l| l.contains(sid))
        .unwrap_or_else(|| panic!("session not in text status:\n{text}"));
    assert!(
        line.contains("worktree (unknown:"),
        "text must say the worktree is unknown: {line}"
    );
    assert!(
        text.contains("/r/legacy-text.rs"),
        "legacy path must be listed in text:\n{text}"
    );
}

#[test]
fn legacy_paths_survive_a_new_event_and_stay_unattributed() {
    let sb = sandbox();
    let sid = "s-legacy-new";
    seed_legacy(&sb, sid, &["/r/old.rs"]);
    let new_file = sb.wt.join("new.rs");
    write(&sb, sid, &sb.wt, &new_file);

    let r = rows(&sb, sid);
    let legacy: Vec<&Value> = r.iter().filter(|x| x["worktree"] == "").collect();
    assert_eq!(legacy.len(), 1, "one unattributed row: {r:?}");
    assert!(legacy[0]["worktree_undetermined"].is_string(), "{r:?}");
    assert_eq!(paths(legacy[0]), vec!["/r/old.rs"]);
    let wt_row = row_for(&r, &sb.wt);
    assert_eq!(paths(wt_row), vec![new_file.to_str().unwrap()]);
    assert!(
        wt_row["worktree_undetermined"].is_null(),
        "new row is determined: {wt_row}"
    );
    let (_, text) = status(&sb, false);
    assert!(text.contains("worktree (unknown:"), "{text}");
}

#[test]
fn legacy_entry_without_writes_is_still_listed() {
    let sb = sandbox();
    let sid = "s-legacy-empty";
    seed_legacy(&sb, sid, &[]);

    let r = rows(&sb, sid);
    assert_eq!(r.len(), 1, "legacy entry must not vanish: {r:?}");
    assert!(paths(&r[0]).is_empty(), "{r:?}");
}

// ---- 4. one process, two worktrees in sequence ------------------------------

#[test]
fn sequential_writes_from_two_worktrees_attributed_to_each_row() {
    let sb = sandbox();
    let sid = "s-seq";
    let a = sb.repo.join("a.rs");
    let b = sb.wt.join("b.rs");
    let c = sb.repo.join("c.rs");
    write(&sb, sid, &sb.repo, &a);
    write(&sb, sid, &sb.wt, &b);
    write(&sb, sid, &sb.repo, &c);

    let r = rows(&sb, sid);
    assert_eq!(r.len(), 2, "{r:?}");
    assert_eq!(
        paths(row_for(&r, &sb.repo)),
        vec![a.to_str().unwrap(), c.to_str().unwrap()]
    );
    assert_eq!(paths(row_for(&r, &sb.wt)), vec![b.to_str().unwrap()]);
}

// ---- 5. pruning is entry-level ----------------------------------------------

fn backdate(sb: &Sandbox, sid: &str, ts: i64) {
    let mut reg = read_reg(sb);
    let e = reg.get_mut(sid).unwrap();
    e["last_seen"] = json!(ts);
    for (_, slot) in e["worktrees"].as_object_mut().unwrap().iter_mut() {
        slot["last_seen"] = json!(ts);
    }
    write_reg(sb, &reg);
}

#[test]
fn old_multi_slot_entry_with_dead_transcript_is_pruned_as_a_whole() {
    let sb = sandbox();
    let sid = "s-prune-dead";
    let ghost = sb.root.join("ghost.jsonl"); // never created => unknown liveness
    let mut p = ev(&sb, sid, "SessionStart", &sb.repo);
    p["transcript_path"] = json!(ghost);
    fire(&sb, &p);
    p["cwd"] = json!(sb.wt);
    fire(&sb, &p);
    assert_eq!(
        read_reg(&sb)[sid]["worktrees"].as_object().unwrap().len(),
        2
    );
    backdate(&sb, sid, now_secs() - H25);

    start(&sb, "trigger", &sb.repo);
    let reg = read_reg(&sb);
    assert!(reg.get(sid).is_none(), "whole entry pruned: {reg}");
    assert!(reg.get("trigger").is_some(), "{reg}");
}

#[test]
fn old_multi_slot_entry_with_live_transcript_is_kept_with_all_slots() {
    let sb = sandbox();
    let sid = "s-prune-live";
    start(&sb, sid, &sb.repo);
    start(&sb, sid, &sb.wt);
    backdate(&sb, sid, now_secs() - H25);

    start(&sb, "trigger", &sb.repo);
    let reg = read_reg(&sb);
    let slots = reg[sid]["worktrees"]
        .as_object()
        .unwrap_or_else(|| panic!("entry pruned although transcript is live: {reg}"));
    assert_eq!(slots.len(), 2, "{reg}");
}

// ---- 6. repo_root cannot be determined --------------------------------------

#[test]
fn cwd_without_git_ancestor_is_keyed_by_cwd_not_a_guessed_worktree() {
    let sb = sandbox();
    let sid = "s-nogit";
    let plain = sb.root.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let plain = plain.canonicalize().unwrap();
    // Precondition: no ancestor of `plain` is a git checkout.
    assert!(
        plain.ancestors().all(|a| !a.join(".git").exists()),
        "test premise: {} has a .git ancestor",
        plain.display()
    );
    let f = plain.join("x.txt");
    write(&sb, sid, &plain, &f);

    // A non-git cwd has its own store (storage_root keys by it), so status is
    // read from that same cwd.
    let r = rows_at(&sb, &plain, sid);
    assert_eq!(r.len(), 1, "{r:?}");
    let plain_row = row_for(&r, &plain);
    assert_eq!(paths(plain_row), vec![f.to_str().unwrap()]);
    assert!(
        plain_row["worktree_undetermined"].is_null(),
        "determined as cwd itself: {plain_row}"
    );
}

#[test]
fn write_from_repo_subdirectory_is_attributed_to_the_repo_root() {
    let sb = sandbox();
    let sid = "s-subdir";
    let sub = sb.repo.join("deep").join("er");
    std::fs::create_dir_all(&sub).unwrap();
    let f = sub.join("z.rs");
    write(&sb, sid, &sub, &f);
    write(&sb, sid, &sb.wt, &sb.wt.join("w.rs"));

    let r = rows(&sb, sid);
    assert_eq!(r.len(), 2, "subdir must not get its own row: {r:?}");
    assert_eq!(paths(row_for(&r, &sb.repo)), vec![f.to_str().unwrap()]);
}
