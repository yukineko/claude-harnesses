//! Claim-reproduction tests for the prose in README.md / README.ja.md and the
//! module docs of parallelguard. Each test observes the real binary against a
//! fresh temp state dir / HOME; nothing touches the real `~/.parallelguard`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_parallelguard");

fn payload_in(session: &str, tool: &str, input: serde_json::Value) -> String {
    serde_json::json!({
        "session_id": session,
        "hook_event_name": "PreToolUse",
        "tool_name": tool,
        "tool_input": input,
    })
    .to_string()
}

fn payload(session: &str, tool: &str, cmd: &str) -> String {
    payload_in(session, tool, serde_json::json!({ "command": cmd }))
}

/// Run the binary. `state`: Some -> PARALLELGUARD_STATE_DIR, None -> unset.
/// `home`: always a temp HOME so the real one is never reachable.
fn run_with(state: Option<&Path>, home: &Path, cap: Option<&str>, sub: &str, body: &str) -> Output {
    let mut c = Command::new(BIN);
    c.arg(sub)
        .env("HOME", home)
        .env_remove("PARALLELGUARD_STATE_DIR")
        .env_remove("HARNESS_MAX_PARALLEL")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(s) = state {
        c.env("PARALLELGUARD_STATE_DIR", s);
    }
    if let Some(v) = cap {
        c.env("HARNESS_MAX_PARALLEL", v);
    }
    let mut child = c.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let _ = stdin.write_all(body.as_bytes());
    drop(stdin);
    child.wait_with_output().unwrap()
}

fn run(state: &Path, cap: Option<&str>, sub: &str, body: &str) -> Output {
    let home = tempfile::tempdir().unwrap();
    run_with(Some(state), home.path(), cap, sub, body)
}

fn out_s(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn is_deny(o: &Output) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(out_s(o).trim()) else {
        return false;
    };
    o.status.code() == Some(0) && v["hookSpecificOutput"]["permissionDecision"] == "deny"
}

/// Sequentially acquire `n` distinct calls; return how many were admitted.
fn admitted(state: &Path, cap: Option<&str>, tool: &str, n: usize) -> usize {
    (0..n)
        .filter(|i| {
            let o = run(state, cap, "acquire", &payload("s", tool, &format!("c{i}")));
            if is_deny(&o) {
                false
            } else {
                assert_eq!(o.status.code(), Some(0));
                assert!(out_s(&o).trim().is_empty(), "{}", out_s(&o));
                true
            }
        })
        .count()
}

fn ledger(state: &Path, session: &str) -> PathBuf {
    state.join("sessions").join(format!("{session}.json"))
}

#[test]
fn claim_lower_cap() {
    for (cap, tool) in [("2", "Bash"), ("1", "Bash"), ("2", "Task"), ("1", "Agent")] {
        let d = tempfile::tempdir().unwrap();
        let want: usize = cap.parse().unwrap();
        assert_eq!(
            admitted(d.path(), Some(cap), tool, 6),
            want,
            "HARNESS_MAX_PARALLEL={cap} {tool}"
        );
    }
}

#[test]
fn claim_ceiling() {
    // Values that must resolve to the ceiling (3).
    for v in ["abc", "", "-1", "4", "99999999999999999999"] {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(
            admitted(d.path(), Some(v), "Bash", 6),
            3,
            "HARNESS_MAX_PARALLEL={v:?} must resolve to the ceiling 3"
        );
    }
}

#[test]
fn claim_ceiling_zero_actual_behaviour() {
    // OBSERVED (contradicts "out-of-range resolves to the ceiling"): "0" is
    // clamped UP to the floor 1, not to the ceiling 3.
    let d = tempfile::tempdir().unwrap();
    assert_eq!(admitted(d.path(), Some("0"), "Bash", 6), 1);
}

#[test]
fn claim_ledger_path() {
    // HOME-relative default.
    let home = tempfile::tempdir().unwrap();
    for s in ["s1", "s2"] {
        let o = run_with(None, home.path(), None, "acquire", &payload(s, "Bash", "x"));
        assert!(out_s(&o).trim().is_empty());
    }
    let base = home.path().join(".parallelguard/state/sessions");
    for s in ["s1", "s2"] {
        let p = base.join(format!("{s}.json"));
        let text = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        serde_json::from_str::<serde_json::Value>(&text).expect("valid JSON");
    }
    // Absolute override: used, and HOME untouched.
    let home2 = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    run_with(
        Some(state.path()),
        home2.path(),
        None,
        "acquire",
        &payload("s1", "Bash", "x"),
    );
    assert!(ledger(state.path(), "s1").is_file());
    assert!(!home2.path().join(".parallelguard").exists());
}

#[test]
fn claim_lock_serializes() {
    // (a) a lockfile sibling of the ledger exists after an acquire.
    let d = tempfile::tempdir().unwrap();
    run(d.path(), None, "acquire", &payload("s", "Bash", "x"));
    let lock = d.path().join("sessions/s.json.lock");
    assert!(lock.is_file(), "lockfile sibling missing");
    assert!(ledger(d.path(), "s").is_file());
}

/// Hold an exclusive flock on `lock` for the lifetime of the returned file.
fn hold(lock: &Path) -> std::fs::File {
    std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock)
        .unwrap();
    f.lock().unwrap();
    f
}

fn still_held(lock: &Path) -> bool {
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(lock)
        .unwrap();
    matches!(f.try_lock(), Err(std::fs::TryLockError::WouldBlock))
}

#[test]
fn claim_lock_held_denies() {
    let d = tempfile::tempdir().unwrap();
    let lock = d.path().join("sessions/s.json.lock");
    let _held = hold(&lock);
    let o = run(d.path(), None, "acquire", &payload("s", "Bash", "x"));
    assert!(
        is_deny(&o),
        "stdout={} stderr={}",
        out_s(&o),
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(lock.exists(), "lockfile was removed");
    assert!(still_held(&lock), "lock was stolen");
    assert!(!ledger(d.path(), "s").exists(), "nothing may be recorded");
}

#[cfg(unix)]
#[test]
fn claim_write_failure_denies() {
    use std::os::unix::fs::PermissionsExt;
    let d = tempfile::tempdir().unwrap();
    // Create the lockfile first, then make the sessions dir read-only so that
    // lock+load succeed (absent ledger) and only save fails.
    drop(hold(&d.path().join("sessions/s.json.lock")));
    let sess = d.path().join("sessions");
    std::fs::set_permissions(&sess, std::fs::Permissions::from_mode(0o555)).unwrap();
    let probe = std::fs::File::create(sess.join("probe"));
    if probe.is_ok() {
        std::fs::set_permissions(&sess, std::fs::Permissions::from_mode(0o755)).unwrap();
        panic!(
            "running with privileges that ignore directory modes; cannot induce a write failure"
        );
    }
    let o = run(d.path(), None, "acquire", &payload("s", "Bash", "x"));
    std::fs::set_permissions(&sess, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        is_deny(&o),
        "stdout={} stderr={}",
        out_s(&o),
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(out_s(&o).contains("could not be recorded"), "{}", out_s(&o));
}

#[test]
fn claim_never_silent() {
    let check = |label: &str, o: &Output| {
        let silent_zero = o.status.code() == Some(0) && out_s(o).trim().is_empty();
        assert!(!silent_zero, "{label}: exit 0 with empty stdout (an allow)");
        if o.status.code() == Some(2) {
            assert!(!o.stderr.is_empty(), "{label}: exit 2 with empty stderr");
        } else {
            assert!(is_deny(o), "{label}: stdout={}", out_s(o));
        }
    };
    // garbage stdin, empty stdin
    let d = tempfile::tempdir().unwrap();
    check("garbage", &run(d.path(), None, "acquire", "not json"));
    check("empty", &run(d.path(), None, "acquire", ""));
    // corrupt ledger
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("sessions")).unwrap();
    std::fs::write(ledger(d.path(), "s"), b"{ trunc").unwrap();
    check(
        "corrupt",
        &run(d.path(), None, "acquire", &payload("s", "Bash", "x")),
    );
    // held lock
    let d = tempfile::tempdir().unwrap();
    let _held = hold(&d.path().join("sessions/s.json.lock"));
    check(
        "held",
        &run(d.path(), None, "acquire", &payload("s", "Bash", "x")),
    );
    // unwritable ledger dir
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        drop(hold(&d.path().join("sessions/s.json.lock")));
        let sess = d.path().join("sessions");
        std::fs::set_permissions(&sess, std::fs::Permissions::from_mode(0o555)).unwrap();
        let o = run(d.path(), None, "acquire", &payload("s", "Bash", "x"));
        std::fs::set_permissions(&sess, std::fs::Permissions::from_mode(0o755)).unwrap();
        check("unwritable", &o);
    }
}

#[test]
fn claim_hooks_wiring() {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("hooks/hooks.json");
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
    let h = &v["hooks"];
    let cmds = |ev: &str| -> Vec<(String, Vec<String>)> {
        h[ev]
            .as_array()
            .unwrap_or_else(|| panic!("{ev} missing"))
            .iter()
            .map(|e| {
                (
                    e["matcher"].as_str().unwrap_or("<none>").to_string(),
                    e["hooks"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|x| x["command"].as_str().unwrap().to_string())
                        .collect(),
                )
            })
            .collect()
    };
    let pre = cmds("PreToolUse");
    assert_eq!(pre.len(), 1);
    assert_eq!(pre[0].0, "Bash|Task|Agent");
    assert_eq!(
        pre[0].1,
        ["${CLAUDE_PLUGIN_ROOT}/bin/parallelguard acquire"]
    );
    let post = cmds("PostToolUse");
    assert_eq!(post.len(), 1);
    assert_eq!(post[0].0, "Bash|Task|Agent");
    assert_eq!(
        post[0].1,
        ["${CLAUDE_PLUGIN_ROOT}/bin/parallelguard release"]
    );
    for ev in ["SessionStart", "UserPromptSubmit", "Stop"] {
        let c = cmds(ev);
        assert_eq!(c.len(), 1, "{ev}");
        assert_eq!(
            c[0].1,
            ["${CLAUDE_PLUGIN_ROOT}/bin/parallelguard reset"],
            "{ev}"
        );
    }
    // Exact matchers: SessionStart is not a catch-all.
    assert_eq!(cmds("SessionStart")[0].0, "startup|resume|clear|compact");
    assert_eq!(cmds("UserPromptSubmit")[0].0, "<none>");
    assert_eq!(cmds("Stop")[0].0, "<none>");
}

/// Kills the holder on drop so a failing assertion never orphans a perl that
/// would keep cargo's stderr pipe open for 300 s.
#[cfg(unix)]
struct Holder(std::process::Child);

#[cfg(unix)]
impl Drop for Holder {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(unix)]
fn spawn_perl_holder(lock: &Path) -> Holder {
    std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
    let child = Command::new("perl")
        .args([
            "-e",
            r#"use Fcntl qw(:flock); open F,">>",shift or die; flock F,LOCK_EX or die; $|=1; print "locked\n"; sleep 300"#,
        ])
        .arg(lock)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("perl is required for this test");
    let mut line = String::new();
    let mut child = Holder(child);
    BufReader::new(child.0.stdout.as_mut().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert_eq!(line.trim(), "locked");
    child
}

#[cfg(unix)]
#[test]
fn claim_killed_holder() {
    // (a) killed holder: lock gone with the process, lockfile persists.
    let d = tempfile::tempdir().unwrap();
    let lock = d.path().join("sessions/s.json.lock");
    let mut h = spawn_perl_holder(&lock);
    h.0.kill().unwrap(); // SIGKILL
    h.0.wait().unwrap();
    let o = run(d.path(), None, "acquire", &payload("s", "Bash", "x"));
    assert_eq!(o.status.code(), Some(0));
    assert!(out_s(&o).trim().is_empty(), "expected allow: {}", out_s(&o));
    assert!(lock.exists());
    let l: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(ledger(d.path(), "s")).unwrap()).unwrap();
    assert_eq!(l["slots"].as_array().map(Vec::len), Some(1), "{l}");

    // (b) live holder with an ancient lockfile mtime: denied, never stolen.
    let d = tempfile::tempdir().unwrap();
    let lock = d.path().join("sessions/s.json.lock");
    let _h = spawn_perl_holder(&lock);
    let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&lock)
        .unwrap()
        .set_modified(old)
        .unwrap();
    let o = run(d.path(), None, "acquire", &payload("s", "Bash", "x"));
    assert!(is_deny(&o), "stdout={}", out_s(&o));
    assert!(lock.exists());
    assert!(still_held(&lock), "a live holder's lock was stolen");
}

#[test]
fn claim_background_not_special() {
    let d = tempfile::tempdir().unwrap();
    let bg = |i: usize| {
        payload_in(
            "s",
            "Bash",
            serde_json::json!({ "command": format!("sleep {i}"), "run_in_background": true }),
        )
    };
    for i in 0..3 {
        let o = run(d.path(), None, "acquire", &bg(i));
        assert!(out_s(&o).trim().is_empty(), "bg call {i}: {}", out_s(&o));
    }
    assert!(is_deny(&run(d.path(), None, "acquire", &bg(3))));
    run(d.path(), None, "release", &bg(0));
    let o = run(d.path(), None, "acquire", &bg(3));
    assert!(
        out_s(&o).trim().is_empty(),
        "release did not return the slot: {}",
        out_s(&o)
    );
}

#[test]
fn claim_status_contents() {
    let d = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    run(d.path(), Some("2"), "acquire", &payload("s", "Bash", "a"));
    let st = |d: &Path| {
        let o = run_with(Some(d), home.path(), Some("2"), "status", "");
        out_s(&o)
    };
    let t = st(d.path());
    assert!(t.contains("cap: 2 concurrent"), "{t}");
    assert!(t.contains("shell 1/2"), "{t}");
    // fill the pool, then one more -> deny
    run(d.path(), Some("2"), "acquire", &payload("s", "Bash", "b"));
    assert!(is_deny(&run(
        d.path(),
        Some("2"),
        "acquire",
        &payload("s", "Bash", "c")
    )));
    let t = st(d.path());
    assert!(t.contains("recent events"), "{t}");
    assert!(
        t.lines().any(|l| l.contains("deny") && l.contains("cap=2")),
        "a refused call must show in status: {t}"
    );
}

#[test]
fn claim_status_distinguishes() {
    let home = tempfile::tempdir().unwrap();
    let empty = tempfile::tempdir().unwrap();
    let t = out_s(&run_with(
        Some(empty.path()),
        home.path(),
        None,
        "status",
        "",
    ));
    assert!(t.contains("the gate is NOT running"), "{t}");

    let d = tempfile::tempdir().unwrap();
    run(d.path(), None, "acquire", &payload("s", "Bash", "a"));
    run(d.path(), None, "release", &payload("s", "Bash", "a"));
    let t = out_s(&run_with(Some(d.path()), home.path(), None, "status", ""));
    assert!(!t.contains("NOT running"), "{t}");
    assert!(t.contains("shell 0/3"), "{t}");
}

#[test]
fn claim_status_after_reset() {
    let home = tempfile::tempdir().unwrap();
    let d = tempfile::tempdir().unwrap();
    let call = payload("s", "Bash", "a");
    run(d.path(), None, "acquire", &call);
    run(d.path(), None, "release", &call);
    assert!(
        ledger(d.path(), "s").is_file(),
        "precondition: ledger exists"
    );
    let stop = serde_json::json!({"session_id": "s", "hook_event_name": "Stop"}).to_string();
    let o = run(d.path(), None, "reset", &stop);
    assert_eq!(o.status.code(), Some(0));
    assert!(
        !ledger(d.path(), "s").exists(),
        "reset must delete the ledger"
    );
    let t = out_s(&run_with(Some(d.path()), home.path(), None, "status", ""));
    assert!(t.contains("the gate is NOT running"), "{t}");
    assert!(!t.contains("shell 0/3"), "{t}");
}
