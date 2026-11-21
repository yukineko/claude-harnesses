#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Regression (2026-11-21): `askrelay mark-relayed <id>` run by an operator in
//! a shell whose stdin was a pipe that never reached EOF hung >120s. The
//! mark-* subcommands are hook entries (no args, JSON on stdin); a call that
//! carries extra positional args is an operator invocation: it must be a usage
//! error (non-zero, stderr non-empty) and must not read stdin at all.

use std::io::Read;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

const DEADLINE: Duration = Duration::from_secs(5);

fn spawn(args: &[&str], stdin: Stdio, home: &std::path::Path) -> Child {
    Command::new(env!("CARGO_BIN_EXE_askrelay"))
        .args(args)
        .env("HOME", home)
        .env("BLASTGUARD_RELAY_DIR", home.join("relay"))
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn askrelay")
}

/// Wait with a deadline; on expiry kill the child and panic naming the hang.
fn wait_bounded(child: &mut Child, what: &str) -> (ExitStatus, String) {
    let start = Instant::now();
    loop {
        if let Some(st) = child.try_wait().expect("try_wait") {
            let mut err = String::new();
            if let Some(mut e) = child.stderr.take() {
                let _ = e.read_to_string(&mut err);
            }
            return (st, err);
        }
        if start.elapsed() > DEADLINE {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "HANG: `{what}` did not exit within {DEADLINE:?} (blocked reading stdin?); killed"
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn home(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("askrelay-markargs-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn check_open_pipe(sub: &str, arg: &str) {
    let h = home(sub);
    let mut child = spawn(&[sub, arg], Stdio::piped(), &h);
    let _keep_open = child.stdin.take().expect("stdin pipe"); // never written, never closed
    let what = format!("askrelay {sub} {arg} (stdin: open pipe)");
    let (st, err) = wait_bounded(&mut child, &what);
    assert!(
        !st.success(),
        "`{what}` exited {st:?}; must be a non-zero usage error"
    );
    assert!(!err.trim().is_empty(), "`{what}` must explain on stderr");
}

#[test]
fn mark_relayed_with_arg_and_open_stdin_pipe_exits_nonzero_bounded() {
    check_open_pipe("mark-relayed", "bgr-720a4ee48f0b");
}

#[test]
fn mark_executed_with_arg_and_open_stdin_pipe_exits_nonzero_bounded() {
    check_open_pipe("mark-executed", "bgr-720a4ee48f0b");
}

#[test]
fn mark_relayed_with_arg_and_null_stdin_exits_nonzero() {
    let h = home("null");
    let mut child = spawn(&["mark-relayed", "bgr-720a4ee48f0b"], Stdio::null(), &h);
    let what = "askrelay mark-relayed <id> (stdin: /dev/null)";
    let (st, _err) = wait_bounded(&mut child, what);
    assert!(
        !st.success(),
        "`{what}` exited {st:?}: silent false success, it ignored the id and marked nothing"
    );
}
