// 統合テスト: テスト内の unwrap/expect/panic は意図的な assert（production 側は workspace の deny のまま）。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED regression (observed 2026-11-16): `presence.json` was found as 32322
//! bytes of all NUL. `presence::save_presence` / `store::save_leases` do
//! `fs::write(tmp)` + `fs::rename(tmp, final)` with NO fsync of the temp file
//! in between, so a crash after the rename can commit the file size while the
//! data blocks were never written (NUL-filled file). The correct sequence is
//! write tmp -> fsync(tmp fd) -> rename (cf. harness-core store.rs).
//!
//! This test OBSERVES the syscall order with `strace -f` on the real binary:
//! for the rename whose destination is the ledger file, an `fsync`/`fdatasync`
//! on the fd that opened the rename source must precede the rename.
//!
//! Non-vacuity controls: the rename of the ledger, the open of its source, and
//! the write to that fd must all be found in the trace; if strace itself is
//! unusable (ptrace denied) the tests FAIL (that is not an acceptable RED).
//!
//! What this does NOT prove: durability across a real power loss (only the
//! syscall ordering of one run), nor the other writers in store.rs
//! (~827 / ~1137 / ~1455), nor directory fsync after rename.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_overwatch");

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

/// Run `overwatch <args>` under strace; return the strace log text.
fn strace_run(sb: &Sandbox, name: &str, args: &[&str], stdin: Option<&str>) -> String {
    let log = sb.root.join(format!("{name}.strace"));
    let mut c = Command::new("/usr/bin/strace");
    harness_core::git_env::scrub(&mut c);
    c.current_dir(&sb.repo)
        .env("HOME", &sb.home)
        .env("PATH", "/usr/bin:/bin")
        .env_remove("HARNESS_SANDBOX_HOME")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("OVERWATCH_RUN_ID")
        .args([
            "-f",
            "-s",
            "4096",
            "-e",
            "trace=openat,write,fsync,fdatasync,rename,renameat,renameat2,close",
            "-o",
        ])
        .arg(&log)
        .arg(BIN)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = c.spawn().expect("cannot spawn /usr/bin/strace");
    {
        use std::io::Write;
        let mut si = child.stdin.take().unwrap();
        if let Some(s) = stdin {
            si.write_all(s.as_bytes()).unwrap();
        }
    }
    let out = child.wait_with_output().unwrap();
    let trace = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        out.status.success() && !trace.is_empty(),
        "strace run of `overwatch {args:?}` failed (exit {:?}); strace unusable/ptrace denied is \
         NOT an acceptable RED.\nstdout:{}\nstderr:{}\ntrace:{trace}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    trace
}

/// First double-quoted string in `s` after `from`, plus the rest of the line.
fn quoted_args(call_args: &str) -> Vec<String> {
    let mut v = Vec::new();
    let mut it = call_args.chars().peekable();
    while let Some(c) = it.next() {
        if c == '"' {
            let mut s = String::new();
            while let Some(d) = it.next() {
                if d == '\\' {
                    if let Some(e) = it.next() {
                        s.push(e);
                    }
                } else if d == '"' {
                    break;
                } else {
                    s.push(d);
                }
            }
            v.push(s);
        }
    }
    v
}

struct Line {
    pid: String,
    call: String,
    args: String,
    ret: Option<i64>,
}

fn parse(trace: &str) -> Vec<Line> {
    let mut out = Vec::new();
    for raw in trace.lines() {
        let (pid, rest) = match raw.split_once(' ') {
            Some((p, r)) if p.chars().all(|c| c.is_ascii_digit()) => (p.to_string(), r.trim()),
            _ => continue,
        };
        let Some(paren) = rest.find('(') else {
            continue;
        };
        let call = rest[..paren].to_string();
        let ret = rest
            .rsplit_once(" = ")
            .and_then(|(_, r)| r.split_whitespace().next())
            .and_then(|r| r.parse::<i64>().ok());
        out.push(Line {
            pid,
            call,
            args: rest[paren + 1..].to_string(),
            ret,
        });
    }
    out
}

/// Assert: for the rename whose destination ends with `dest_suffix`, the
/// source was opened, written, and fsync'd (fsync/fdatasync on that fd, same
/// pid) strictly before the rename.
fn assert_fsync_before_rename(trace: &str, dest_suffix: &str) {
    let lines = parse(trace);
    let (ri, src) = lines
        .iter()
        .enumerate()
        .find_map(|(i, l)| {
            if !matches!(l.call.as_str(), "rename" | "renameat" | "renameat2") {
                return None;
            }
            let q = quoted_args(&l.args);
            // rename(old,new) / renameat*(fd,old,fd,new,..): last two strings.
            if q.len() >= 2 && l.ret == Some(0) && q[q.len() - 1].ends_with(dest_suffix) {
                Some((i, q[q.len() - 2].clone()))
            } else {
                None
            }
        })
        .unwrap_or_else(|| {
            panic!("CONTROL FAILED: no successful rename onto *{dest_suffix} in trace:\n{trace}")
        });
    let rename_pid = lines[ri].pid.clone();
    let (oi, fd) = lines[..ri]
        .iter()
        .enumerate()
        .rev()
        .find_map(|(i, l)| {
            if l.call == "openat"
                && l.pid == rename_pid
                && quoted_args(&l.args).first().map(String::as_str) == Some(src.as_str())
            {
                l.ret.filter(|r| *r >= 0).map(|fd| (i, fd.to_string()))
            } else {
                None
            }
        })
        .unwrap_or_else(|| {
            panic!("CONTROL FAILED: rename source {src} was never opened in trace:\n{trace}")
        });
    assert!(
        lines[oi..ri].iter().any(|l| l.call == "write"
            && l.pid == rename_pid
            && l.args.starts_with(&format!("{fd},"))),
        "CONTROL FAILED: no write to fd {fd} ({src}) before the rename:\n{trace}"
    );
    let synced = lines[oi..ri].iter().any(|l| {
        matches!(l.call.as_str(), "fsync" | "fdatasync")
            && l.pid == rename_pid
            && l.args.starts_with(&format!("{fd})"))
    });
    assert!(
        synced,
        "MISSING FSYNC: temp file {src} (fd {fd}) was renamed onto *{dest_suffix} without \
         fsync/fdatasync on that fd first; a crash after the rename can leave an all-NUL \
         file (observed 2026-11-16 presence.json 32322 bytes of NUL).\ntrace:\n{trace}"
    );
}

#[test]
fn presence_json_temp_is_fsynced_before_rename() {
    let sb = sandbox();
    let payload = serde_json::json!({
        "hook_event_name": "SessionStart",
        "session_id": "t1",
        "cwd": sb.repo.to_str().unwrap(),
        "transcript_path": sb.root.join("t.jsonl").to_str().unwrap(),
    })
    .to_string();
    let trace = strace_run(&sb, "presence", &["presence"], Some(&payload));
    assert_fsync_before_rename(&trace, "presence.json");
}

#[test]
fn leases_json_temp_is_fsynced_before_rename() {
    let sb = sandbox();
    let trace = strace_run(
        &sb,
        "begin",
        &["begin", "--key", "k1", "--title", "t", "--session", "s1"],
        None,
    );
    assert_fsync_before_rename(&trace, "leases.json");
}
