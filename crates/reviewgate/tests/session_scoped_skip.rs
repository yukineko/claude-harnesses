// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! MIRROR of `crates/donegate/tests/session_scoped_skip.rs`, for reviewgate.
//!
//! Written because the ONE gate that was first driven end-to-end besides
//! donegate (propguard) turned up a real wiring bug — its `skip` CLI resolved a
//! different state dir than its Stop hook, so the hatch reported being armed
//! while the hook could not see it. A source-level scan cannot catch that
//! class: both sides referenced the right primitive. Only running the two
//! halves against each other does.
//!
//! The defect being pinned shut is the same one: `consume_skip(&root,
//! ".reviewgate-skip")` read an unattributed file from the SHARED project root,
//! so whichever session's Stop hook fired next consumed it — that session's
//! legitimate gate waved through on an exception it never asked for, and the
//! session that created the marker left without its exception (CLAUDE.md §5).
//!
//! Written by a DISINTERESTED author (CLAUDE.md §2(a)).
//!
//! ## Isolation
//!
//! Each test gets its own directory under `std::env::temp_dir()`, passed to the
//! child process as `HOME` and used as its cwd. reviewgate's default state dir
//! is `$HOME/.reviewgate/state`, so a per-child `HOME` moves all of its durable
//! state into the scratch dir. No process-global environment variable is
//! mutated and the live `~/.reviewgate` store is never read or written.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn scratch(tag: &str) -> PathBuf {
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("reviewgate-skip-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch");
    dir
}

/// A git repo with one committed file and one unreviewed change — the state in
/// which reviewgate (mode `inject`, `min_changed_files = 1` by default) has
/// something to review and therefore something to block on.
fn project(tag: &str) -> PathBuf {
    let dir = scratch(tag);
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "t@t"],
        vec!["config", "user.name", "t"],
    ] {
        let _ = Command::new("git").current_dir(&dir).args(&args).output();
    }
    std::fs::write(dir.join("seed.rs"), "fn a() {}\n").expect("write seed");
    for args in [vec!["add", "-A"], vec!["commit", "-qm", "seed"]] {
        let _ = Command::new("git").current_dir(&dir).args(&args).output();
    }
    std::fs::write(
        dir.join("changed.rs"),
        "fn f() {\n    let x = 1;\n    println!(\"{}\", x);\n}\n",
    )
    .expect("write change");
    dir
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Run {
    fn blocked(&self) -> bool {
        self.stdout.contains("\"decision\"") && self.stdout.contains("block")
    }
}

fn run(dir: &Path, args: &[&str], session: Option<&str>, stdin: &str) -> Run {
    let bin = env!("CARGO_BIN_EXE_reviewgate");
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env_remove("REVIEWGATE_DISABLE")
        .env_remove("HARNESS_TRUST_ALL");
    match session {
        Some(s) => {
            cmd.env("CLAUDE_CODE_SESSION_ID", s);
        }
        None => {
            cmd.env_remove("CLAUDE_CODE_SESSION_ID");
        }
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("reviewgate spawns");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("reviewgate runs");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn stop(dir: &Path, session: &str) -> Run {
    let payload = serde_json::json!({
        "session_id": session,
        "cwd": dir.to_string_lossy(),
        "hook_event_name": "Stop",
    })
    .to_string();
    run(dir, &["review"], Some(session), &payload)
}

fn issue_skip(dir: &Path, session: &str, reason: &str) -> Run {
    run(dir, &["skip", "--reason", reason], Some(session), "")
}

/// ANTI-VACUITY CONTROL: with no skip of any kind, reviewgate blocks. Every
/// "must block" assertion below is meaningless without this.
#[test]
fn with_no_skip_at_all_reviewgate_still_blocks() {
    let dir = project("control");
    let r = stop(&dir, "sess-control");
    assert_eq!(r.code, 0, "a Stop hook always exits 0 toward Claude");
    assert!(
        r.blocked(),
        "apparatus: reviewgate must block here, else this whole file proves nothing; stdout={:?} \
         stderr={:?}",
        r.stdout,
        r.stderr
    );
}

/// An unattributed marker in the shared project root must not wave reviewgate's
/// stop through.
#[test]
fn an_unattributed_project_root_marker_no_longer_waves_a_reviewgate_stop_through() {
    let dir = project("shared-marker");
    std::fs::write(
        dir.join(".reviewgate-skip"),
        "left here by some other session\n",
    )
    .expect("write the legacy shared marker");

    let r = stop(&dir, "sess-victim");
    assert_eq!(r.code, 0);
    assert!(
        r.blocked(),
        "a file in the SHARED project root waved this session's reviewgate stop through — the \
         mirror gap. stdout={:?} stderr={:?}",
        r.stdout,
        r.stderr
    );
}

/// The two-session form, through the supported CLI. This is the shape that
/// caught propguard's state-dir split: it exercises the `skip` CLI and the Stop
/// hook against each other, so the two halves must agree on where a skip lives.
#[test]
fn a_skip_issued_by_another_session_does_not_wave_my_reviewgate_gate_through() {
    let dir = project("cross-session");

    let issued = issue_skip(&dir, "sess-A", "landing a doc-only fix");
    assert_eq!(
        issued.code, 0,
        "`reviewgate skip --reason …` must succeed for the issuing session; stdout={:?} \
         stderr={:?}",
        issued.stdout, issued.stderr
    );

    let b = stop(&dir, "sess-B");
    assert!(
        b.blocked(),
        "session B's legitimate reviewgate gate was waved through by a skip session A asked for. \
         stdout={:?} stderr={:?}",
        b.stdout,
        b.stderr
    );

    let a = stop(&dir, "sess-A");
    assert!(
        !a.blocked(),
        "session A's own skip did not survive session B's stop. If the `skip` CLI and the Stop \
         hook resolve different state dirs, the hatch reports being armed while the hook cannot \
         see it — the bug this shape found in propguard. stdout={:?} stderr={:?}",
        a.stdout,
        a.stderr
    );
}
