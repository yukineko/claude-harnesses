#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED(ask-learning): `overwatch ask-learn` — the learner that turns real
//! interactive Ask outcomes into blastguard's learned table.
//!
//! Spec: docs/specs/blastguard-ask-learning.md (user ruling 2026-11-28),
//! component 3. Written by an agent that does NOT implement it (CLAUDE.md
//! §2(a)). Black-box: spawns the overwatch binary and a built blastguard.
//!
//! Fixtures are produced the way production produces them, not hand-written:
//!   * blocks.jsonl lines come from running the REAL blastguard hook in
//!     interactive mode with BLASTGUARD_BLOCK_LOG_DIR pointed at the fixture
//!     (so the record schema is whatever blastguard writes today);
//!   * transcripts are `<projects>/<proj>/<sid>.jsonl` files with an assistant
//!     `tool_use` (same tool_name + tool_input as the hook payload) and a user
//!     `tool_result`; the decline text is the one Claude Code writes.
//! Shape keys are never hard-coded: they come from `blastguard shape`.
//!
//! HOME is isolated per test: the spec puts status.json / promotions.jsonl
//! under `~/.blastguard/learned/`, so these tests read them from
//! `$HOME/.blastguard/learned/`.
//!
//! blastguard binary: see tests/support/blastguard_bin.rs
//! (`ASK_LEARN_BLASTGUARD_BIN` or a one-time `cargo build -p blastguard`).
//!
//! What these tests do NOT prove: the Stop hook's time budget and its
//! systemMessage rendering of status.json (only the hooks.json wiring is
//! checked); `file-reds` on a linked worktree WITH pending promotions (that
//! path calls the real `backlog add`, which a test must not do); behaviour on
//! real-size logs (676 records) or the cursor's crash safety.

#[path = "support/blastguard_bin.rs"]
mod blastguard_bin;

use serde_json::{json, Value};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

const DECLINE: &str = "The user doesn't want to proceed with this tool use. The tool use was \
rejected (eg. if it was a file edit, the new_string was NOT written to the file). STOP what you \
are doing and wait for the user to tell you how to proceed.";

const S1: &str = "7a1e0c55-0001-4d2e-9f00-00000000aaa1";
const S2: &str = "7a1e0c55-0002-4d2e-9f00-00000000aaa2";
const S3: &str = "7a1e0c55-0003-4d2e-9f00-00000000aaa3";
const CWD: &str = "/tmp/ask-learn-fixture-cwd";

static TOOL_ID: AtomicU64 = AtomicU64::new(1);

struct Fx {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    blocks_dir: PathBuf,
    projects: PathBuf,
    table: PathBuf,
}

impl Fx {
    fn new() -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let f = Fx {
            home: root.join("home"),
            blocks_dir: root.join("blocks"),
            projects: root.join("projects"),
            table: root.join("learned").join("table.json"),
            root,
            _tmp: tmp,
        };
        for d in [&f.home, &f.blocks_dir, &f.projects.join("-tmp-fixture")] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::create_dir_all(f.table.parent().unwrap()).unwrap();
        f
    }
    fn blocks(&self) -> PathBuf {
        self.blocks_dir.join("blocks.jsonl")
    }
    fn learned_dir(&self) -> PathBuf {
        self.home.join(".blastguard").join("learned")
    }
}

fn tool_input(cmd: &str) -> Value {
    json!({ "command": cmd, "description": "fixture" })
}

fn payload(sid: &str, cmd: &str) -> Value {
    json!({
        "hook_event_name": "PreToolUse",
        "session_id": sid,
        "cwd": CWD,
        "tool_name": "Bash",
        "tool_input": tool_input(cmd),
    })
}

fn bg(f: &Fx, args: &[&str], stdin: &str) -> Output {
    let mut c = Command::new(blastguard_bin::blastguard_bin());
    c.args(args)
        .env("HOME", &f.home)
        .env("BLASTGUARD_BLOCK_LOG_DIR", &f.blocks_dir)
        .env("BLASTGUARD_RELAY_DIR", f.root.join("relay"))
        .env("BLASTGUARD_APPROVALS_DIR", f.root.join("approvals"))
        .env(
            "BLASTGUARD_LEARNED_TABLE",
            f.root.join("hook-sees-no-table.json"),
        )
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("BLASTGUARD_ASK")
        .env_remove("CLAUDE_PROJECT_DIR")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    std::fs::create_dir_all(f.root.join("approvals")).unwrap();
    let mut child = c.spawn().expect("spawn blastguard");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

/// Run the real hook so blastguard itself appends the Ask record.
fn record_ask(f: &Fx, sid: &str, cmd: &str) {
    let out = bg(f, &[], &payload(sid, cmd).to_string());
    let s = String::from_utf8_lossy(&out.stdout);
    let v: Value = serde_json::from_str(s.trim()).unwrap_or_else(|e| {
        panic!("fixture: blastguard printed no decision ({e}) for {cmd:?}: {s:?}")
    });
    assert_eq!(
        v["hookSpecificOutput"]["permissionDecision"], "ask",
        "fixture drift: {cmd:?} must be an interactive Ask today: {v}"
    );
}

fn shape_key(f: &Fx, cmd: &str) -> String {
    let out = bg(f, &["shape"], &payload(S1, cmd).to_string());
    assert!(
        out.status.success(),
        "`blastguard shape` failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    v["shape"].as_str().unwrap().to_string()
}

fn transcript(f: &Fx, sid: &str) -> PathBuf {
    f.projects.join("-tmp-fixture").join(format!("{sid}.jsonl"))
}

fn append(path: &Path, v: Value) {
    let mut fh = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    writeln!(fh, "{}", serde_json::to_string(&v).unwrap()).unwrap();
}

fn tool_use(f: &Fx, sid: &str, cmd: &str) -> String {
    let id = format!("toolu_fx{:06}", TOOL_ID.fetch_add(1, Ordering::SeqCst));
    append(
        &transcript(f, sid),
        json!({
            "type": "assistant",
            "sessionId": sid,
            "cwd": CWD,
            "message": { "role": "assistant", "content": [
                { "type": "tool_use", "id": id, "name": "Bash", "input": tool_input(cmd) }
            ]}
        }),
    );
    id
}

fn tool_result(f: &Fx, sid: &str, id: &str, content: &str, is_error: bool) {
    append(
        &transcript(f, sid),
        json!({
            "type": "user",
            "sessionId": sid,
            "cwd": CWD,
            "message": { "role": "user", "content": [
                { "type": "tool_result", "tool_use_id": id, "content": content, "is_error": is_error }
            ]}
        }),
    );
}

/// An Ask the user approved; the command then ran (ok or errored).
fn approved(f: &Fx, sid: &str, cmd: &str, errored: bool) {
    record_ask(f, sid, cmd);
    let id = tool_use(f, sid, cmd);
    if errored {
        tool_result(
            f,
            sid,
            &id,
            "Exit code 1\nrm: : No such file or directory",
            true,
        );
    } else {
        tool_result(f, sid, &id, "", false);
    }
}

fn declined(f: &Fx, sid: &str, cmd: &str) {
    record_ask(f, sid, cmd);
    let id = tool_use(f, sid, cmd);
    tool_result(f, sid, &id, DECLINE, true);
}

fn ask_learn(f: &Fx, extra: &[&str]) -> Output {
    let bin = blastguard_bin::blastguard_bin();
    ask_learn_with(f, &f.blocks(), &bin, extra)
}

fn ask_learn_with(f: &Fx, blocks: &Path, bin: &Path, extra: &[&str]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_overwatch"));
    c.arg("ask-learn")
        .arg("--blocks")
        .arg(blocks)
        .arg("--projects")
        .arg(&f.projects)
        .arg("--table")
        .arg(&f.table)
        .arg("--blastguard")
        .arg(bin)
        .args(extra)
        .env("HOME", &f.home)
        .env("BLASTGUARD_BLOCK_LOG_DIR", &f.blocks_dir)
        .env_remove("BLASTGUARD_LEARNED_TABLE")
        .stdin(Stdio::null());
    c.output().expect("spawn overwatch")
}

fn ok(out: &Output) {
    assert!(
        out.status.success(),
        "ask-learn must succeed: code={:?} stdout={} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The learned table. A run that learned nothing may or may not create the
/// file (the spec does not say), so an ABSENT file reads as "no entries" here;
/// every test that expects an entry still fails on its absence.
fn table(f: &Fx) -> Value {
    if !f.table.exists() {
        return json!({ "version": 1, "entries": [] });
    }
    let s = std::fs::read_to_string(&f.table)
        .unwrap_or_else(|e| panic!("table {} not written: {e}", f.table.display()));
    let v: Value = serde_json::from_str(&s).unwrap();
    assert_eq!(v["version"], 1, "{v}");
    v
}

fn entry<'a>(t: &'a Value, key: &str) -> Option<&'a Value> {
    t["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["shape"] == key)
}

fn promotions(f: &Fx) -> Vec<Value> {
    match std::fs::read_to_string(f.learned_dir().join("promotions.jsonl")) {
        Ok(s) => s
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect(),
        Err(_) => vec![],
    }
}

/// S1×2 (one errored) + S2×1: 3 approvals, 2 sessions, 0 declines.
fn seed_allow(f: &Fx) {
    approved(f, S1, r#"rm "$f1""#, false);
    approved(f, S1, r#"rm "$f2""#, true);
    approved(f, S2, r#"rm "$f3""#, false);
}

// ------------------------------------------------------------ thresholds

#[test]
fn three_approvals_two_sessions_learn_allow() {
    let f = Fx::new();
    seed_allow(&f);
    let key = shape_key(&f, r#"rm "$f1""#);
    assert_eq!(
        key,
        shape_key(&f, r#"rm "$f3""#),
        "fixture: commands must share one shape"
    );
    let out = ask_learn(&f, &[]);
    ok(&out);
    let t = table(&f);
    let e = entry(&t, &key).unwrap_or_else(|| panic!("no entry for {key}: {t}"));
    assert_eq!(e["decision"], "allow", "{e}");
    assert_eq!(
        e["approvals"], 3,
        "an errored run is still an approval: {e}"
    );
    assert_eq!(e["declines"], 0, "{e}");
    assert_eq!(e["sessions"], 2, "{e}");
    let so = String::from_utf8_lossy(&out.stdout);
    assert!(
        so.contains("allow: 1"),
        "summary must report allow count: {so}"
    );
}

#[test]
fn two_approvals_learn_nothing() {
    let f = Fx::new();
    approved(&f, S1, r#"rm "$f1""#, false);
    approved(&f, S2, r#"rm "$f2""#, false);
    let key = shape_key(&f, r#"rm "$f1""#);
    ok(&ask_learn(&f, &[]));
    assert!(entry(&table(&f), &key).is_none(), "{}", table(&f));
}

#[test]
fn decline_learns_deny() {
    let f = Fx::new();
    seed_allow(&f);
    approved(&f, S3, r#"rm "$f4""#, false);
    declined(&f, S3, r#"rm "$f5""#);
    let key = shape_key(&f, r#"rm "$f1""#);
    ok(&ask_learn(&f, &[]));
    let t = table(&f);
    let e = entry(&t, &key).unwrap_or_else(|| panic!("no entry: {t}"));
    assert_eq!(e["decision"], "deny", "{e}");
    assert_eq!(e["approvals"], 4, "{e}");
    assert_eq!(e["declines"], 1, "{e}");
}

// ------------------------------------------------------------ exclusions

#[test]
fn pseudo_sessions_are_excluded() {
    let f = Fx::new();
    approved(&f, S1, r#"rm "$f1""#, false);
    // Would make 4 approvals across 4 "sessions" if counted.
    approved(&f, "probe", r#"rm "$f2""#, false);
    approved(&f, "verify-abc", r#"rm "$f3""#, false);
    approved(&f, "v2", r#"rm "$f6""#, false);
    let key = shape_key(&f, r#"rm "$f1""#);
    ok(&ask_learn(&f, &[]));
    assert!(entry(&table(&f), &key).is_none(), "{}", table(&f));
    // A decline in a pseudo session must not produce a deny either.
    let g = Fx::new();
    declined(&g, "probe", r#"rm "$f7""#);
    ok(&ask_learn(&g, &[]));
    assert!(entry(&table(&g), &key).is_none(), "{}", table(&g));
}

#[test]
fn unmatched_asks_are_excluded() {
    let f = Fx::new();
    approved(&f, S1, r#"rm "$f1""#, false);
    approved(&f, S1, r#"rm "$f2""#, false);
    // S2: the Ask was recorded but the transcript holds a DIFFERENT input.
    record_ask(&f, S2, r#"rm "$f3""#);
    let id = tool_use(&f, S2, r#"rm "$other""#);
    tool_result(&f, S2, &id, "", false);
    // S3: tool_use present, no tool_result yet.
    record_ask(&f, S3, r#"rm "$f4""#);
    tool_use(&f, S3, r#"rm "$f4""#);
    // A session with an Ask but no transcript file at all.
    record_ask(&f, "7a1e0c55-0009-4d2e-9f00-00000000aaa9", r#"rm "$f5""#);
    let key = shape_key(&f, r#"rm "$f1""#);
    ok(&ask_learn(&f, &[]));
    assert!(
        entry(&table(&f), &key).is_none(),
        "only S1's two approvals are matched; nothing may be learned: {}",
        table(&f)
    );
}

#[test]
fn records_no_longer_asked_are_skipped() {
    let f = Fx::new();
    seed_allow(&f); // positive control in the same run
                    // Re-label three historical records as `ls` (Allow today), approved in 2 sessions.
    let lines: Vec<Value> = std::fs::read_to_string(f.blocks())
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    for (i, sid) in [S1, S2, S3].iter().enumerate() {
        let cmd = format!("ls -la /tmp/n{i}");
        let mut r = lines[0].clone();
        r["session_id"] = json!(sid);
        r["tool_input"] = tool_input(&cmd);
        r["command"] = json!(cmd);
        append(&f.blocks(), r);
        let id = tool_use(&f, sid, &cmd);
        tool_result(&f, sid, &id, "", false);
    }
    let key = shape_key(&f, r#"rm "$f1""#);
    ok(&ask_learn(&f, &[]));
    let t = table(&f);
    assert!(entry(&t, &key).is_some(), "positive control missing: {t}");
    let es = t["entries"].as_array().unwrap();
    assert_eq!(es.len(), 1, "the `ls` records must be skipped: {t}");
    assert!(!t.to_string().contains("ls -la"), "{t}");
}

// ------------------------------------------------------------ writing the table

#[test]
fn unchanged_content_is_not_rewritten() {
    let f = Fx::new();
    seed_allow(&f);
    ok(&ask_learn(&f, &[]));
    let before = std::fs::read(&f.table).unwrap();
    let m1 = std::fs::metadata(&f.table).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    ok(&ask_learn(&f, &[]));
    ok(&ask_learn(&f, &["--full"]));
    let m2 = std::fs::metadata(&f.table).unwrap().modified().unwrap();
    assert_eq!(m1, m2, "unchanged table must not be rewritten");
    assert_eq!(before, std::fs::read(&f.table).unwrap());
    let names: Vec<String> = std::fs::read_dir(f.table.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        ["table.json"],
        "atomic write must leave no tmp files: {names:?}"
    );
}

#[test]
fn incremental_equals_full_rebuild() {
    let f = Fx::new();
    approved(&f, S1, r#"rm "$f1""#, false);
    approved(&f, S1, r#"rm "$f2""#, false);
    let key = shape_key(&f, r#"rm "$f1""#);
    ok(&ask_learn(&f, &[]));
    assert!(entry(&table(&f), &key).is_none());
    approved(&f, S2, r#"rm "$f3""#, false);
    ok(&ask_learn(&f, &[])); // incremental
    let inc = std::fs::read(&f.table).unwrap();
    assert_eq!(
        entry(&table(&f), &key).map(|e| e["decision"].clone()),
        Some(json!("allow")),
        "incremental run must count the new approval: {}",
        table(&f)
    );
    std::fs::remove_file(&f.table).unwrap();
    ok(&ask_learn(&f, &["--full"]));
    assert_eq!(
        inc,
        std::fs::read(&f.table).unwrap(),
        "incremental ≠ full rebuild"
    );
}

#[test]
fn failure_keeps_previous_table_and_reports() {
    let f = Fx::new();
    seed_allow(&f);
    ok(&ask_learn(&f, &[]));
    let before = std::fs::read(&f.table).unwrap();
    let status = f.learned_dir().join("status.json");

    // (1) blastguard missing.
    let missing = f.root.join("nonexistent-blastguard");
    let out = ask_learn_with(&f, &f.blocks(), &missing, &["--full"]);
    assert!(
        !out.status.success(),
        "missing blastguard must exit non-zero"
    );
    assert_eq!(
        before,
        std::fs::read(&f.table).unwrap(),
        "previous table must survive"
    );
    let st = std::fs::read_to_string(&status)
        .unwrap_or_else(|e| panic!("status.json not written at {}: {e}", status.display()));
    let st: Value = serde_json::from_str(&st).expect("status.json is JSON");
    assert!(
        !st.as_object().map_or(true, |o| o.is_empty()),
        "status must carry the error: {st}"
    );
    std::fs::remove_file(&status).unwrap();

    // (2) blocks log unreadable (a directory where the file should be).
    let bad = f.root.join("blocks-is-a-dir");
    std::fs::create_dir_all(&bad).unwrap();
    let out = ask_learn_with(&f, &bad, &blastguard_bin::blastguard_bin(), &["--full"]);
    assert!(
        !out.status.success(),
        "unreadable blocks log must exit non-zero"
    );
    assert_eq!(before, std::fs::read(&f.table).unwrap());
    assert!(status.is_file(), "status.json must record the failure");
}

// ------------------------------------------------------------ promotions

#[test]
fn new_allow_is_appended_to_promotions_once() {
    let f = Fx::new();
    seed_allow(&f);
    let key = shape_key(&f, r#"rm "$f1""#);
    ok(&ask_learn(&f, &[]));
    let p = promotions(&f);
    assert_eq!(p.len(), 1, "{p:?}");
    assert_eq!(p[0]["shape"], key.as_str(), "{}", p[0]);
    assert_eq!(p[0]["filed"], false, "{}", p[0]);
    ok(&ask_learn(&f, &["--full"]));
    assert_eq!(
        promotions(&f).len(),
        1,
        "an existing allow is not a new promotion"
    );
}

#[test]
fn deny_is_not_a_promotion() {
    let f = Fx::new();
    declined(&f, S1, r#"rm "$f1""#);
    ok(&ask_learn(&f, &[]));
    assert!(promotions(&f).is_empty(), "{:?}", promotions(&f));
}

// ------------------------------------------------------------ file-reds

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn file_reds(f: &Fx, worktree: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_overwatch"))
        .args(["ask-learn", "file-reds", "--worktree"])
        .arg(worktree)
        .env("HOME", &f.home)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn learned_shape_files(repo: &Path) -> Vec<String> {
    match std::fs::read_dir(repo.join("crates/blastguard/tests")) {
        Ok(rd) => rd
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("learned_shape_"))
            .collect(),
        Err(_) => vec![],
    }
}

#[test]
fn file_reds_refuses_main_working_tree() {
    // Main tree with a pending promotion: must refuse and leave it unfiled.
    let f = Fx::new();
    seed_allow(&f);
    ok(&ask_learn(&f, &[]));
    assert_eq!(promotions(&f).len(), 1, "fixture: one pending promotion");
    let main = f.root.join("repo");
    std::fs::create_dir_all(&main).unwrap();
    git(&main, &["init", "-q"]);
    git(&main, &["commit", "-q", "--allow-empty", "-m", "init"]);
    let out = file_reds(&f, &main);
    assert!(
        !out.status.success(),
        "file-reds on the main working tree must refuse: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        learned_shape_files(&main).is_empty(),
        "nothing may be written to the main tree"
    );
    assert!(
        promotions(&f).iter().all(|p| p["filed"] == false),
        "a refused run must not mark promotions filed: {:?}",
        promotions(&f)
    );

    // Control (anti-vacuity): a linked worktree with NO pending promotions is
    // accepted (exit 0). No promotion → no `backlog add` is ever invoked.
    let g = Fx::new();
    let linked = f.root.join("linked");
    git(
        &main,
        &["worktree", "add", "-q", "-b", "l", linked.to_str().unwrap()],
    );
    let out = file_reds(&g, &linked);
    assert!(
        out.status.success(),
        "file-reds on a linked worktree with nothing to file must succeed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(learned_shape_files(&linked).is_empty());
}

// ------------------------------------------------------------ Stop hook wiring

#[test]
fn stop_hook_runs_ask_learn() {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("hooks")
        .join("hooks.json");
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    let stop = v["hooks"]["Stop"]
        .as_array()
        .unwrap_or_else(|| panic!("no Stop hook in {}", p.display()));
    let hooks: Vec<&Value> = stop
        .iter()
        .flat_map(|g| g["hooks"].as_array().into_iter().flatten())
        .filter(|h| {
            h["command"]
                .as_str()
                .is_some_and(|c| c.contains("overwatch ask-learn"))
        })
        .collect();
    assert_eq!(
        hooks.len(),
        1,
        "exactly one Stop hook runs `overwatch ask-learn`: {v}"
    );
    assert!(
        hooks[0]["timeout"].is_number(),
        "the Stop run needs a time budget: {}",
        hooks[0]
    );
}
