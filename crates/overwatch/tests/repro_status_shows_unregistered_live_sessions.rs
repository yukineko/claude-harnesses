// 統合テスト: テスト内の unwrap/expect/panic は意図的な assert（production 側は workspace の deny のまま）。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro: `overwatch status` does not show live sessions that never ran
//! `overwatch begin`.
//!
//! Black-box contract (presence-contract.md): the ONLY way these tests
//! register a session is by running the commands configured in
//! `crates/overwatch/hooks/hooks.json` with Claude Code hook payloads on stdin.
//! Observation goes only through `overwatch status` and `overwatch status --json`.
//! `${CLAUDE_PLUGIN_ROOT}/bin/overwatch` is replaced by the built binary and
//! every child runs with `HOME` = a tempdir sandbox, so nothing touches the real
//! `~/.overwatch`. `PATH` is `/usr/bin:/bin` (git only): missing helper binaries
//! (backlog / hypothesis / condukt / compass) make those sections undetermined,
//! which is irrelevant to the Sessions assertions here.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, SystemTime};
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_overwatch");
const PLUGIN_BIN: &str = "${CLAUDE_PLUGIN_ROOT}/bin/overwatch";

struct Sandbox {
    _t: TempDir,
    root: PathBuf,
    home: PathBuf,
    repo: PathBuf,
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
    Sandbox {
        _t: t,
        root,
        home,
        repo,
    }
}

fn base_cmd(sb: &Sandbox, cwd: &Path) -> Command {
    let mut c = Command::new("sh");
    harness_core::git_env::scrub(&mut c);
    c.current_dir(cwd)
        .env("HOME", &sb.home)
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDE_PLUGIN_ROOT", env!("CARGO_MANIFEST_DIR"))
        .env_remove("HARNESS_SANDBOX_HOME")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("OVERWATCH_RUN_ID");
    c
}

/// Commands configured in hooks.json for `event`; for tool events only the
/// entries whose `matcher` regex matches `tool` (no/empty matcher = all).
fn hook_commands(event: &str, tool: Option<&str>) -> Vec<String> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("hooks/hooks.json");
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    let mut out = Vec::new();
    let Some(entries) = v["hooks"][event].as_array() else {
        return out;
    };
    for e in entries {
        let m = e.get("matcher").and_then(Value::as_str).unwrap_or("");
        if let (Some(tool), false) = (tool, m.is_empty()) {
            let re = regex::Regex::new(&format!("^(?:{m})$")).unwrap();
            if !re.is_match(tool) {
                continue;
            }
        }
        for h in e["hooks"].as_array().cloned().unwrap_or_default() {
            if h["type"] == "command" {
                out.push(h["command"].as_str().unwrap().replace(PLUGIN_BIN, BIN));
            }
        }
    }
    out
}

/// Fire every hooks.json command for `event` with `payload` on stdin, cwd =
/// payload cwd. Each well-formed payload's hook command must exit 0.
fn fire(sb: &Sandbox, event: &str, tool: Option<&str>, payload: &Value) {
    let cwd = PathBuf::from(payload["cwd"].as_str().unwrap());
    for cmd in hook_commands(event, tool) {
        let mut child = base_cmd(sb, &cwd)
            .arg("-c")
            .arg(&cmd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        {
            use std::io::Write;
            let mut stdin = child.stdin.take().unwrap();
            stdin.write_all(payload.to_string().as_bytes()).unwrap();
        }
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "{event} hook command `{cmd}` exited {:?} for a well-formed payload\nstdout:{}\nstderr:{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

fn session_start(sb: &Sandbox, sid: &str, transcript: &Path, cwd: &Path) {
    fire(
        sb,
        "SessionStart",
        None,
        &json!({"session_id":sid,"transcript_path":transcript,"cwd":cwd,
                "hook_event_name":"SessionStart","source":"startup"}),
    );
}

fn post_edit(sb: &Sandbox, sid: &str, transcript: &Path, cwd: &Path, file: &Path) {
    fire(
        sb,
        "PostToolUse",
        Some("Edit"),
        &json!({"session_id":sid,"transcript_path":transcript,"cwd":cwd,
                "hook_event_name":"PostToolUse","tool_name":"Edit",
                "tool_input":{"file_path":file,"old_string":"a","new_string":"b"},
                "tool_response":{"filePath":file,"success":true}}),
    );
}

fn session_end(sb: &Sandbox, sid: &str, transcript: &Path, cwd: &Path) {
    fire(
        sb,
        "SessionEnd",
        None,
        &json!({"session_id":sid,"transcript_path":transcript,"cwd":cwd,
                "hook_event_name":"SessionEnd","reason":"exit"}),
    );
}

fn run_status(sb: &Sandbox, cwd: &Path, json_flag: bool) -> Output {
    let mut c = Command::new(BIN);
    harness_core::git_env::scrub(&mut c);
    c.current_dir(cwd)
        .env("HOME", &sb.home)
        .env("PATH", "/usr/bin:/bin")
        .env_remove("HARNESS_SANDBOX_HOME")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("OVERWATCH_RUN_ID")
        .stdin(Stdio::null())
        .arg("status");
    if json_flag {
        c.arg("--json");
    }
    c.output().unwrap()
}

fn status_json(sb: &Sandbox, cwd: &Path) -> (Value, String) {
    let out = run_status(sb, cwd, true);
    let raw = String::from_utf8_lossy(&out.stdout).to_string();
    let v: Value = serde_json::from_str(&raw).unwrap_or_else(|e| {
        panic!(
            "status --json stdout is not JSON ({e}):\n{raw}\nstderr:{}",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    (v, raw)
}

/// Text between `== Sessions ==` and the next `== ` header.
fn sessions_section(sb: &Sandbox, cwd: &Path) -> (String, String) {
    let out = run_status(sb, cwd, false);
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let start = text
        .find("== Sessions ==")
        .unwrap_or_else(|| panic!("no `== Sessions ==` header in status:\n{text}"));
    let body = &text[start + "== Sessions ==".len()..];
    let end = body.find("\n== ").unwrap_or(body.len());
    (body[..end].trim().to_string(), text)
}

fn active_entry<'a>(v: &'a Value, sid: &str) -> Option<&'a Value> {
    v.get("active_sessions")?
        .as_array()?
        .iter()
        .find(|e| e["session_id"] == sid)
}

fn canon(p: &str) -> PathBuf {
    Path::new(p)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(p))
}

fn write_transcript(path: &Path, age_secs: u64) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, "{\"type\":\"user\"}\n").unwrap();
    set_age(path, age_secs);
}

fn set_age(path: &Path, age_secs: u64) {
    let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_modified(SystemTime::now() - Duration::from_secs(age_secs))
        .unwrap();
}

/// Proves: a session registered ONLY through the hooks.json SessionStart and
/// PostToolUse(Edit) commands, run from a LINKED worktree, is visible to
/// `overwatch status` / `status --json` run in the MAIN checkout, with its
/// session id, cwd/worktree = the linked worktree, the edited file in
/// `written_paths`, and liveness "live" (fresh transcript). The control call
/// before any hook proves the session id cannot come from elsewhere (e.g. a
/// transcript scan) and also primes the status cache, so the post-hook call
/// proves the cache does not hide a later registration.
///
/// Does NOT prove: behaviour under real Claude Code (payloads are synthetic),
/// the Write/MultiEdit/NotebookEdit tool shapes, path dedup, `last_seen`
/// values, or concurrent hook writers.
#[test]
fn live_session_from_linked_worktree_is_listed_in_main_checkout() {
    let sb = sandbox();
    let wt = sb.root.join("wt-linked");
    git(
        &sb.repo,
        &["worktree", "add", "-q", "-b", "feat", wt.to_str().unwrap()],
    );
    let wt = wt.canonicalize().unwrap();
    let sid = "sess-live-7d1c0e5a";
    let transcript = sb.root.join("transcripts").join(format!("{sid}.jsonl"));
    write_transcript(&transcript, 0);
    let file = wt.join("src").join("lib.rs");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "b\n").unwrap();

    // Control (anti-vacuity): before any hook the session id appears nowhere.
    let (_, raw0) = status_json(&sb, &sb.repo);
    assert!(
        !raw0.contains(sid),
        "session id visible before any hook ran — it is not coming from the hooks:\n{raw0}"
    );

    assert!(
        !hook_commands("SessionStart", None).is_empty(),
        "hooks.json has no SessionStart command"
    );
    session_start(&sb, sid, &transcript, &wt);
    post_edit(&sb, sid, &transcript, &wt, &file);
    // The hooks must not have aged the transcript; keep it fresh regardless.
    set_age(&transcript, 0);

    let (v, raw) = status_json(&sb, &sb.repo);
    let e = active_entry(&v, sid).unwrap_or_else(|| {
        panic!("status --json (main checkout) has no active_sessions entry for {sid}:\n{raw}")
    });
    assert_eq!(
        canon(e["cwd"].as_str().unwrap_or("")),
        wt,
        "active_sessions cwd is not the linked worktree:\n{e}"
    );
    assert_eq!(
        canon(e["worktree"].as_str().unwrap_or("")),
        wt,
        "active_sessions worktree is not the linked worktree:\n{e}"
    );
    let written: Vec<PathBuf> = e["written_paths"]
        .as_array()
        .unwrap_or_else(|| panic!("written_paths is not an array:\n{e}"))
        .iter()
        .map(|p| canon(p.as_str().unwrap_or("")))
        .collect();
    assert!(
        written.contains(&file),
        "written_paths lacks the edited file {}:\n{e}",
        file.display()
    );
    assert_eq!(e["liveness"], "live", "fresh transcript must be live:\n{e}");

    let (sec, text) = sessions_section(&sb, &sb.repo);
    assert_ne!(sec, "(none)", "Sessions section says (none):\n{text}");
    assert!(sec.contains(sid), "Sessions section lacks {sid}:\n{text}");
    let wt_s = wt.to_string_lossy();
    assert!(
        sec.contains(wt_s.as_ref()),
        "Sessions section lacks worktree {wt_s}:\n{text}"
    );
    let file_s = file.to_string_lossy();
    let file_rel = "src/lib.rs";
    assert!(
        sec.contains(file_s.as_ref()) || sec.contains(file_rel),
        "Sessions section lacks the written path {file_s}:\n{text}"
    );
}

/// Proves: liveness is not blanket "live". A 2-hour-old transcript (beyond the
/// 1800 s TTL) is listed but not "live"; a session that got SessionEnd is
/// absent or "ended", never "live"; a session whose transcript_path does not
/// exist is listed with liveness "unknown" (not dropped, not live). A fresh
/// control session in the same store proves the roster is actually populated
/// (otherwise "absent" would pass vacuously for the ended session).
///
/// Does NOT prove: the exact TTL boundary, "stale" vs another non-live label
/// for the old session, or how long ended sessions are retained.
#[test]
fn stale_ended_and_unknown_sessions_are_not_live() {
    let sb = sandbox();
    let tdir = sb.root.join("transcripts");
    let cwd = sb.repo.clone();

    let live = "sess-ctl-live-0a1b";
    let t_live = tdir.join("live.jsonl");
    write_transcript(&t_live, 0);
    session_start(&sb, live, &t_live, &cwd);

    let stale = "sess-stale-2c3d";
    let t_stale = tdir.join("stale.jsonl");
    write_transcript(&t_stale, 0);
    session_start(&sb, stale, &t_stale, &cwd);
    set_age(&t_stale, 2 * 3600);

    let ended = "sess-ended-4e5f";
    let t_ended = tdir.join("ended.jsonl");
    write_transcript(&t_ended, 0);
    session_start(&sb, ended, &t_ended, &cwd);
    session_end(&sb, ended, &t_ended, &cwd);
    set_age(&t_ended, 0);

    let unknown = "sess-unknown-6a7b";
    let t_missing = tdir.join("does-not-exist.jsonl");
    session_start(&sb, unknown, &t_missing, &cwd);
    assert!(!t_missing.exists());
    set_age(&t_live, 0);

    let (v, raw) = status_json(&sb, &cwd);
    let e_live = active_entry(&v, live)
        .unwrap_or_else(|| panic!("control live session {live} missing:\n{raw}"));
    assert_eq!(
        e_live["liveness"], "live",
        "control must be live:\n{e_live}"
    );

    let e_stale = active_entry(&v, stale)
        .unwrap_or_else(|| panic!("stale session {stale} must still be listed:\n{raw}"));
    assert_ne!(
        e_stale["liveness"], "live",
        "2h-old transcript must not be live:\n{e_stale}"
    );
    assert!(
        e_stale["liveness"].is_string(),
        "stale liveness missing:\n{e_stale}"
    );

    if let Some(e_end) = active_entry(&v, ended) {
        assert_eq!(
            e_end["liveness"], "ended",
            "SessionEnd'ed session must be ended or absent, never live:\n{e_end}"
        );
    }

    let e_unk = active_entry(&v, unknown)
        .unwrap_or_else(|| panic!("missing-transcript session {unknown} was dropped:\n{raw}"));
    assert_eq!(
        e_unk["liveness"], "unknown",
        "missing transcript must be unknown:\n{e_unk}"
    );
}

/// Proves: once a session is registered, corrupting every `*.json` file in the
/// overwatch store that mentions the session id makes `status --json` report
/// `undetermined` source "active_sessions" and keeps the text Sessions section
/// off `(none)` — a broken registry is not rendered as "nobody is here".
/// The "at least one such file" assertion keeps it from passing vacuously.
///
/// Does NOT prove: behaviour for registries stored in non-`.json` files,
/// permission-denied (vs corrupt) registries, or the wording of the reason.
#[test]
fn corrupt_presence_registry_is_undetermined_not_none() {
    let sb = sandbox();
    let sid = "sess-corrupt-8c9d";
    let transcript = sb.root.join("transcripts").join("c.jsonl");
    write_transcript(&transcript, 0);
    session_start(&sb, sid, &transcript, &sb.repo);
    set_age(&transcript, 0);

    let store_base = sb.home.join(".overwatch");
    let mut corrupted = Vec::new();
    let mut stack = vec![store_base.clone()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for ent in rd.flatten() {
            let p = ent.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "json")
                && p.components().any(|c| c.as_os_str() == "overwatch")
                && std::fs::read_to_string(&p).is_ok_and(|s| s.contains(sid))
            {
                std::fs::write(&p, "{not json").unwrap();
                corrupted.push(p);
            }
        }
    }
    assert!(
        !corrupted.is_empty(),
        "no *.json file under {} mentions {sid}: the session was never registered by the hooks",
        store_base.display()
    );

    let (v, raw) = status_json(&sb, &sb.repo);
    let und = v["undetermined"].as_array().cloned().unwrap_or_default();
    assert!(
        und.iter().any(|u| u["source"] == "active_sessions"),
        "corrupt registry ({corrupted:?}) not reported as undetermined active_sessions:\n{raw}"
    );
    let (sec, text) = sessions_section(&sb, &sb.repo);
    assert_ne!(
        sec, "(none)",
        "corrupt registry rendered as (none):\n{text}"
    );
}
