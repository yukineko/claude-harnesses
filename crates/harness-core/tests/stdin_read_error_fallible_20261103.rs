// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Contract (user ruling 2026-11-03, backlog 8004d80f; CLAUDE.md §3): a stdin
//! READ ERROR is a cannot-determine. It must surface as a failure, never as an
//! empty or partial `Ok` payload.
//!
//! Pinned here:
//! * `try_read_capped` / `try_read_if_piped` return `Err` on a read error,
//!   whether it happens before any byte or after a partial read; on success they
//!   keep the `MAX_STDIN_BYTES` cap and the lossy decode; the terminal
//!   short-circuit returns `Ok("")` without reading.
//! * `gate::run::read_stop_payload_guarded` returns an `Ok` payload unchanged,
//!   and on `Err` resolves like a `run_guarded` panic: chain-first block JSON
//!   naming the stdin read failure; a second consecutive failure of the same
//!   gate in the same session (marker present) is the bounded allow; without a
//!   session id every such stop blocks; interactive mode exits 1.
//!
//! `read_stop_payload_guarded` ends in `process::exit` on `Err`, so those cases
//! are observed out of process by re-executing this test binary into the
//! `child_*` fns below (no-ops unless `STDIN_FALLIBLE_CHILD` is set), with an
//! isolated HOME/XDG and a controlled `CLAUDE_CODE_SESSION_ID` — the same
//! pattern as `tests/panic_bound_consecutive.rs`.
//!
//! Written by a disinterested author (CLAUDE.md §2(a)); `src/` untouched.

use harness_core::hook::{try_read_capped, try_read_if_piped, MAX_STDIN_BYTES};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

// ---------------------------------------------------------------------------
// Pure reader tests
// ---------------------------------------------------------------------------

/// Serves `head`, then fails every further read.
struct ErrAfter {
    head: Vec<u8>,
}

impl Read for ErrAfter {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.head.is_empty() {
            return Err(std::io::Error::other("synthetic stdin read failure"));
        }
        let n = self.head.len().min(buf.len());
        buf[..n].copy_from_slice(&self.head[..n]);
        self.head.drain(..n);
        Ok(n)
    }
}

/// A reader whose `read` must never be called.
struct PanicOnRead;

impl Read for PanicOnRead {
    fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
        panic!("reader was touched although stdin is a terminal");
    }
}

#[test]
fn try_read_capped_is_err_when_the_reader_fails_before_any_byte() {
    let got = try_read_capped(ErrAfter { head: Vec::new() });
    match got {
        Err(e) => assert!(
            e.to_string().contains("synthetic stdin read failure"),
            "the Err must carry the reader's error, got: {e}"
        ),
        Ok(s) => panic!("a read error before any byte must be Err, got Ok({s:?})"),
    }
}

#[test]
fn try_read_capped_is_err_not_the_prefix_when_the_reader_fails_midway() {
    let got = try_read_capped(ErrAfter {
        head: b"partial".to_vec(),
    });
    match got {
        Err(e) => assert!(
            e.to_string().contains("synthetic stdin read failure"),
            "the Err must carry the reader's error, got: {e}"
        ),
        Ok(s) => panic!("a read error after a partial read must be Err, got Ok({s:?})"),
    }
}

#[test]
fn try_read_capped_returns_the_full_content_on_success() {
    let got = try_read_capped(&b"{\"session_id\":\"abc\"}"[..]).unwrap();
    assert_eq!(got, "{\"session_id\":\"abc\"}");
}

#[test]
fn try_read_capped_decodes_invalid_utf8_lossily() {
    let got = try_read_capped(&[b'a', 0xFF, b'b'][..]).unwrap();
    assert_eq!(got, "a\u{FFFD}b");
}

#[test]
fn try_read_capped_truncates_at_max_stdin_bytes() {
    let over = std::io::repeat(b'x').take(MAX_STDIN_BYTES + 17);
    let got = try_read_capped(over).unwrap();
    assert_eq!(
        got.len() as u64,
        MAX_STDIN_BYTES,
        "input past MAX_STDIN_BYTES must be truncated at the cap"
    );
}

#[test]
fn try_read_if_piped_on_a_terminal_is_ok_empty_without_reading() {
    let got = try_read_if_piped(true, PanicOnRead).unwrap();
    assert_eq!(got, "");
}

#[test]
fn try_read_if_piped_propagates_a_read_error_when_piped() {
    match try_read_if_piped(false, ErrAfter { head: Vec::new() }) {
        Err(e) => assert!(e.to_string().contains("synthetic stdin read failure")),
        Ok(s) => panic!("a piped read error must be Err, got Ok({s:?})"),
    }
}

#[test]
fn try_read_if_piped_propagates_a_midway_read_error_when_piped() {
    match try_read_if_piped(
        false,
        ErrAfter {
            head: b"partial".to_vec(),
        },
    ) {
        Err(_) => {}
        Ok(s) => panic!("a piped midway read error must be Err, got Ok({s:?})"),
    }
}

#[test]
fn try_read_if_piped_returns_content_when_piped() {
    assert_eq!(
        try_read_if_piped(false, &b"payload"[..]).unwrap(),
        "payload"
    );
}

// ---------------------------------------------------------------------------
// read_stop_payload_guarded
// ---------------------------------------------------------------------------

#[test]
fn read_stop_payload_guarded_returns_an_ok_payload_unchanged() {
    // Ok never consults the marker store nor exits, so it runs in-process.
    let got = harness_core::gate::run::read_stop_payload_guarded(
        "stdin-fallible-ok-gate",
        false,
        Ok("{\"stop_hook_active\":false}".to_string()),
    );
    assert_eq!(got, "{\"stop_hook_active\":false}");
}

const CHILD_ENV: &str = "STDIN_FALLIBLE_CHILD";
const GATE: &str = "stdin-fallible-gate";
const SESSION: &str = "s-stdin-fallible-20261103";
/// Printed by a child when `read_stop_payload_guarded` RETURNED on `Err`
/// (it must never return there).
const RETURNED: &str = "STDIN_FALLIBLE_RETURNED:";

/// Re-exec target: hook-mode gate whose stdin read failed.
#[test]
fn child_read_error() {
    if std::env::var(CHILD_ENV).as_deref() != Ok("err") {
        return;
    }
    let p = harness_core::gate::run::read_stop_payload_guarded(
        GATE,
        false,
        Err(std::io::Error::other("synthetic-eio-on-fd0")),
    );
    println!("{RETURNED}{p:?}");
}

/// Re-exec target: interactive gate whose stdin read failed.
#[test]
fn child_read_error_interactive() {
    if std::env::var(CHILD_ENV).as_deref() != Ok("err-interactive") {
        return;
    }
    let p = harness_core::gate::run::read_stop_payload_guarded(
        GATE,
        true,
        Err(std::io::Error::other("synthetic-eio-on-fd0")),
    );
    println!("{RETURNED}{p:?}");
}

/// Re-exec target: the same gate panicking under run_guarded on a chain-first
/// stop (shares the per-gate, per-session marker).
#[test]
fn child_run_guarded_panic() {
    if std::env::var(CHILD_ENV).as_deref() != Ok("panic") {
        return;
    }
    harness_core::gate::run::run_guarded(GATE, false, false, || -> () {
        panic!("deterministic gate panic (stdin_read_error_fallible probe)")
    });
}

fn scratch(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let d = std::env::temp_dir().join(format!(
        "hc-stdin-fallible-{tag}-{}-{nanos}",
        std::process::id()
    ));
    for sub in ["home", "state", "data", "config", "cache", "cwd"] {
        std::fs::create_dir_all(d.join(sub)).unwrap();
    }
    d
}

fn run_child(root: &Path, kind: &str, test: &str, with_session: bool) -> Output {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(CHILD_ENV, kind)
        .env("HOME", root.join("home"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .current_dir(root.join("cwd"));
    if with_session {
        cmd.env("CLAUDE_CODE_SESSION_ID", SESSION);
    } else {
        cmd.env_remove("CLAUDE_CODE_SESSION_ID");
    }
    cmd.output().unwrap()
}

fn err_stop(root: &Path, with_session: bool) -> Output {
    run_child(root, "err", "child_read_error", with_session)
}

fn stdout_of(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

fn stderr_of(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

fn dbg(o: &Output) -> String {
    format!(
        "exit={:?}\nstdout={}\nstderr={}",
        o.status.code(),
        stdout_of(o),
        stderr_of(o)
    )
}

/// The block decision JSON on stdout, if any (parsed from the first `{` of a
/// line: libtest `--nocapture` prints `test <name> ... ` without a newline).
fn block_reason(o: &Output) -> Option<String> {
    stdout_of(o).lines().find_map(|l| {
        let start = l.find('{')?;
        let v: serde_json::Value = serde_json::from_str(l[start..].trim()).ok()?;
        (v.get("decision")?.as_str()? == "block")
            .then(|| v.get("reason")?.as_str().map(str::to_string))?
    })
}

fn returned(o: &Output) -> bool {
    stdout_of(o).contains(RETURNED)
}

#[test]
fn read_error_on_a_chain_first_stop_blocks_naming_the_stdin_failure() {
    let root = scratch("chain-first");
    let o = err_stop(&root, true);
    assert!(
        !returned(&o),
        "read_stop_payload_guarded must not return a payload on Err\n{}",
        dbg(&o)
    );
    assert_eq!(
        o.status.code(),
        Some(0),
        "block protocol is exit 0\n{}",
        dbg(&o)
    );
    let reason = block_reason(&o)
        .unwrap_or_else(|| panic!("a chain-first stdin read error must block\n{}", dbg(&o)));
    assert!(
        reason.contains("stdin read failed"),
        "the block reason must name the stdin read failure, got: {reason}"
    );
    assert!(
        reason.contains("synthetic-eio-on-fd0"),
        "the block reason must carry the io error, got: {reason}"
    );
    assert!(
        reason.contains(GATE),
        "the block reason must name the gate, got: {reason}"
    );
}

#[test]
fn second_consecutive_read_error_of_the_same_gate_is_the_bounded_allow() {
    let root = scratch("bounded");
    let first = err_stop(&root, true);
    assert!(
        block_reason(&first).is_some(),
        "first must block\n{}",
        dbg(&first)
    );
    let second = err_stop(&root, true);
    assert!(
        !returned(&second),
        "must not return a payload on Err\n{}",
        dbg(&second)
    );
    assert_eq!(second.status.code(), Some(0), "{}", dbg(&second));
    assert!(
        block_reason(&second).is_none(),
        "the second consecutive failure (marker present) is the bounded allow\n{}",
        dbg(&second)
    );
    // The streak was consumed by the allow and re-recorded; a third failure
    // keeps being allowed (decide records on BoundedAllow) — not asserted here.
}

#[test]
fn read_error_after_a_run_guarded_panic_of_the_same_gate_is_the_bounded_allow() {
    let root = scratch("after-panic");
    let p = run_child(&root, "panic", "child_run_guarded_panic", true);
    assert!(
        block_reason(&p).is_some(),
        "the run_guarded panic stop must block\n{}",
        dbg(&p)
    );
    let o = err_stop(&root, true);
    assert_eq!(o.status.code(), Some(0), "{}", dbg(&o));
    assert!(
        !returned(&o) && block_reason(&o).is_none(),
        "a stdin read error right after a panic of the same gate shares the marker \
         and is the bounded allow\n{}",
        dbg(&o)
    );
}

#[test]
fn read_error_without_a_session_id_blocks_even_after_a_recorded_failure() {
    let root = scratch("other-session");
    let first = err_stop(&root, true);
    assert!(block_reason(&first).is_some(), "{}", dbg(&first));
    // Same HOME (marker store), no session id: no per-session key, so block.
    let o = err_stop(&root, false);
    assert!(
        block_reason(&o).is_some(),
        "without a session id the marker cannot be looked up; the stop must block\n{}",
        dbg(&o)
    );
}

#[test]
fn read_error_without_a_session_id_blocks_every_time() {
    let root = scratch("no-session");
    for i in 0..2 {
        let o = err_stop(&root, false);
        assert!(
            !returned(&o),
            "must not return a payload on Err\n{}",
            dbg(&o)
        );
        let reason = block_reason(&o).unwrap_or_else(|| {
            panic!(
                "stop #{i} without a session id must block (never an allow)\n{}",
                dbg(&o)
            )
        });
        assert!(reason.contains("stdin read failed"), "got: {reason}");
    }
}

#[test]
fn interactive_read_error_exits_1_naming_the_failure() {
    let root = scratch("interactive");
    let o = run_child(
        &root,
        "err-interactive",
        "child_read_error_interactive",
        true,
    );
    assert!(
        !returned(&o),
        "must not return a payload on Err\n{}",
        dbg(&o)
    );
    assert_eq!(o.status.code(), Some(1), "{}", dbg(&o));
    assert!(
        stderr_of(&o).contains("stdin read failed"),
        "interactive stderr must name the stdin read failure\n{}",
        dbg(&o)
    );
    assert!(block_reason(&o).is_none(), "{}", dbg(&o));
}
