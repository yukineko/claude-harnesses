// 統合テスト: テスト内の unwrap/expect/panic は意図的な assert（production 側は workspace の deny のまま）。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (hypothesis): `presence::apply_event` prunes every entry whose
//! `last_seen` is older than `PRUNE_AFTER_SECS` (24h) on EVERY write by ANY
//! session. `last_seen` only refreshes on that session's own hooks, so a
//! session whose transcript is still fresh (live per the presence contract)
//! but that fired no hook for >24h is deleted from `presence.json` by another
//! session's write and vanishes from `overwatch status`.
//!
//! Contract (presence-contract.md): liveness is judged from transcript mtime;
//! a live session must never be silently dropped.
//!
//! Sessions are registered only via the hooks.json commands (BIN substituted),
//! HOME is a tempdir sandbox. The only white-box step is rewriting
//! `last_seen` in the sandbox `presence.json` to simulate 25h without hooks.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_overwatch");
const PLUGIN_BIN: &str = "${CLAUDE_PLUGIN_ROOT}/bin/overwatch";
const H25: u64 = 25 * 3600;

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

/// Commands configured in hooks.json for `event` (no tool matcher needed:
/// only SessionStart is fired here).
fn hook_commands(event: &str) -> Vec<String> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("hooks/hooks.json");
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    let mut out = Vec::new();
    let Some(entries) = v["hooks"][event].as_array() else {
        return out;
    };
    for e in entries {
        for h in e["hooks"].as_array().cloned().unwrap_or_default() {
            if h["type"] == "command" {
                out.push(h["command"].as_str().unwrap().replace(PLUGIN_BIN, BIN));
            }
        }
    }
    out
}

fn fire(sb: &Sandbox, event: &str, payload: &Value) {
    let cwd = PathBuf::from(payload["cwd"].as_str().unwrap());
    let cmds = hook_commands(event);
    assert!(!cmds.is_empty(), "hooks.json has no {event} command");
    for cmd in cmds {
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
            "{event} hook command `{cmd}` exited {:?}\nstdout:{}\nstderr:{}",
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
        &json!({"session_id":sid,"transcript_path":transcript,"cwd":cwd,
                "hook_event_name":"SessionStart","source":"startup"}),
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

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// The single `presence.json` under `<HOME>/.overwatch/<key>/overwatch/`.
fn find_presence_json(sb: &Sandbox) -> PathBuf {
    let base = sb.home.join(".overwatch");
    let mut found = Vec::new();
    let mut stack = vec![base.clone()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for ent in rd.flatten() {
            let p = ent.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.file_name().is_some_and(|n| n == "presence.json")
                && p.parent()
                    .and_then(Path::file_name)
                    .is_some_and(|n| n == "overwatch")
            {
                found.push(p);
            }
        }
    }
    assert_eq!(
        found.len(),
        1,
        "expected exactly one presence.json under {}: {found:?}",
        base.display()
    );
    found.pop().unwrap()
}

/// Rewrite `last_seen` of each listed session to `ts`, then read the file
/// back and assert the edit took effect.
fn backdate_last_seen(path: &Path, sids: &[&str], ts: i64) {
    let mut reg: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    for sid in sids {
        let e = reg
            .get_mut(*sid)
            .unwrap_or_else(|| panic!("{sid} not in presence.json before backdating"));
        assert!(
            e.get("last_seen").is_some_and(Value::is_i64),
            "{sid} entry has no integer last_seen: {e}"
        );
        e["last_seen"] = json!(ts);
    }
    std::fs::write(path, serde_json::to_string_pretty(&reg).unwrap()).unwrap();
    let back: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    for sid in sids {
        assert_eq!(
            back[*sid]["last_seen"],
            json!(ts),
            "backdating {sid} did not take effect:\n{back}"
        );
    }
}

/// Proves (if GREEN): a session A whose transcript is fresh (live) but whose
/// `last_seen` is 25h old survives another session B's registry write, and
/// is still listed as live in `status --json` and in the text Sessions
/// section — even with an un-live 25h-old entry C beside it (C may be pruned;
/// nothing is asserted about C). B's presence in both outputs proves the
/// write happened and status reads the registry (anti-vacuity).
///
/// Does NOT prove: behaviour under real Claude Code payloads, PostToolUse /
/// SessionEnd writes as the pruning trigger, or the exact 24h boundary.
#[test]
fn live_session_with_old_last_seen_survives_other_sessions_write() {
    let sb = sandbox();
    let cwd = sb.repo.clone();
    let tdir = sb.root.join("transcripts");

    let a = "sess-live-old-lastseen-a1";
    let t_a = tdir.join("a.jsonl");
    write_transcript(&t_a, 0);
    session_start(&sb, a, &t_a, &cwd);

    let c = "sess-dead-old-c3";
    let t_c = tdir.join("c.jsonl");
    write_transcript(&t_c, 0);
    session_start(&sb, c, &t_c, &cwd);

    let presence = find_presence_json(&sb);
    backdate_last_seen(&presence, &[a, c], now_secs() - H25 as i64);

    // A: transcript fresh (live). C: transcript 25h old (not live).
    set_age(&t_a, 0);
    set_age(&t_c, H25);

    let b = "sess-writer-b2";
    let t_b = tdir.join("b.jsonl");
    write_transcript(&t_b, 0);
    session_start(&sb, b, &t_b, &cwd);
    set_age(&t_a, 0);
    set_age(&t_b, 0);

    let (v, raw) = status_json(&sb, &cwd);
    // Control (a): B's write happened and status reads the registry.
    let e_b = active_entry(&v, b)
        .unwrap_or_else(|| panic!("control: writer session {b} missing:\n{raw}"));
    assert_eq!(e_b["liveness"], "live", "control B must be live:\n{e_b}");

    let e_a = active_entry(&v, a).unwrap_or_else(|| {
        panic!(
            "live session {a} (fresh transcript, last_seen 25h ago) was dropped from \
             active_sessions after {b}'s write:\n{raw}\npresence.json now:\n{}",
            std::fs::read_to_string(&presence).unwrap_or_default()
        )
    });
    assert_eq!(e_a["liveness"], "live", "A has a fresh transcript:\n{e_a}");

    let (sec, text) = sessions_section(&sb, &cwd);
    assert!(
        sec.contains(b),
        "control: Sessions section lacks {b}:\n{text}"
    );
    assert!(
        sec.contains(a),
        "Sessions section lacks live session {a}:\n{text}"
    );
}
