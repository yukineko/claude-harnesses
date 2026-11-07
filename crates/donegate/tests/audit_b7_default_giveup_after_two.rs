// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Audit B item 7 (memory `ask-policy`, "Give up after two identical blocks",
//! user ruling 2026-11-07: 「ブロック回数の断念は2回でダメならあきらめて別対応すること」).
//!
//! With NO `max_attempts` configured, donegate must block a persistently red
//! Stop at most twice and give up VISIBLY on the third: the stop is allowed
//! and the give-up sentinel `donegate:giveup:<check>` is written to the
//! overwatch violation ledger. Fixture and helpers are copied from
//! `giveup_sentinel.rs`, minus its explicit `max_attempts = 3`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn scratch(tag: &str) -> PathBuf {
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("donegate-auditb7-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch");
    dir
}

/// A trusted project rooted under an isolated `$HOME`, carrying exactly one
/// required check running `cmd`. Returns (home, project root).
///
/// The project MUST be trusted or `Config::load` ignores `donegate.toml`
/// outright and the gate has no checks at all — the test would then pass
/// vacuously on a gate that never ran anything.
fn project(tag: &str, cmd: &str) -> (PathBuf, PathBuf) {
    let home = scratch(tag);
    let root = home.join("project");
    std::fs::create_dir_all(&root).expect("create project root");

    std::fs::write(
        root.join("donegate.toml"),
        format!("[[check]]\nname = \"typecheck\"\ncmd = \"{cmd}\"\n"),
    )
    .expect("write donegate.toml");

    // `trust::is_trusted` canonicalizes before comparing, so the seeded entry
    // must be the canonical form (on macOS $TMPDIR resolves through /private).
    let canon = std::fs::canonicalize(&root).expect("canonicalize project root");
    std::fs::create_dir_all(home.join(".harness")).expect("create ~/.harness");
    std::fs::write(
        home.join(".harness/trust.toml"),
        format!("trusted = [\"{}\"]\n", canon.display()),
    )
    .expect("write trust.toml");

    (home, root)
}

struct GateRun {
    code: i32,
    stdout: String,
    stderr: String,
}

impl GateRun {
    fn blocked(&self) -> bool {
        self.stdout.contains("\"decision\"") && self.stdout.contains("block")
    }

    /// The repeat ledger's announced second-occurrence waiver
    /// (`harness_core::repeat::emit_stop_block`): a `systemMessage`, NOT a
    /// `decision`, so the stop proceeds while the finding is still reported.
    fn waived_as_repeat(&self) -> bool {
        !self.blocked() && self.stdout.contains("systemMessage") && self.stdout.contains("2度目")
    }
}

/// One Stop hook invocation for `session`.
///
/// `CLAUDE_CODE_SESSION_ID` is set to the SAME `session` as the payload, the
/// way a real Claude Code session delivers both. It must not be inherited:
/// the repeat ledger (`harness_core::repeat`, operator ruling 2026-09-18)
/// keys on this env var, not on the payload, so an inherited value made the
/// fixture's session boundaries meaningless for it — every "session" in this
/// file shared whichever session ran `cargo test` (red inside a Claude Code
/// session, green in a bare shell where the ledger is Undetermined and every
/// block stands).
fn run_gate(home: &Path, root: &Path, session: &str) -> GateRun {
    let bin = env!("CARGO_BIN_EXE_donegate");
    let payload = format!(
        r#"{{"session_id":"{session}","cwd":"{}","hook_event_name":"Stop"}}"#,
        root.display()
    );
    let mut child = harness_core::git_env::scrub(&mut Command::new(bin))
        .arg("gate")
        .current_dir(root)
        .env("HOME", home)
        .env("CLAUDE_CODE_SESSION_ID", session)
        .env_remove("DONEGATE_DISABLE")
        .env_remove("HARNESS_TRUST_ALL")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("donegate spawns");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("write payload");
    let out = child.wait_with_output().expect("donegate runs");
    GateRun {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// Concatenated contents of every `violations.jsonl` under the isolated
/// `$HOME`. Read straight off disk rather than through `overwatch::store` so
/// the observation does not depend on the code under test, and located by
/// walking rather than by recomputing the project-key hash.
fn violation_ledger(home: &Path) -> String {
    fn walk(dir: &Path, out: &mut String) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.file_name().is_some_and(|n| n == "violations.jsonl") {
                out.push_str(&std::fs::read_to_string(&p).unwrap_or_default());
            }
        }
    }
    let mut out = String::new();
    walk(home, &mut out);
    out
}

#[test]
#[ignore = "backlog f500c25a: open defect, remove ignore when fixed"]
fn default_attempt_cap_gives_up_visibly_on_the_third_stop() {
    let (home, root) = project("default-cap", "exit 1");
    let session = "sess-default-cap";

    // CONTROL: the gate enforces on the first two stops. The second may be
    // either a block or the announced repeat waiver (backlog b80023b3), but
    // never a silent allow.
    let r1 = run_gate(&home, &root, session);
    assert!(
        r1.blocked(),
        "control: stop 1 must block; stdout={:?}",
        r1.stdout
    );
    let r2 = run_gate(&home, &root, session);
    assert!(
        r2.blocked() || r2.waived_as_repeat(),
        "control: stop 2 must block or be announced; stdout={:?}",
        r2.stdout
    );
    assert!(
        !violation_ledger(&home).contains("giveup"),
        "control: no give-up sentinel may exist after two stops"
    );

    // THE FINDING: the third stop on the same red check must give up.
    let r3 = run_gate(&home, &root, session);
    assert_eq!(r3.code, 0, "a Stop hook always exits 0 toward Claude");
    assert!(
        !r3.blocked(),
        "stop 3 blocked again: the default cap allows more than two blocks; stdout={:?} stderr={:?}",
        r3.stdout,
        r3.stderr
    );
    assert!(
        violation_ledger(&home).contains("donegate:giveup:typecheck"),
        "stop 3 was allowed without the durable give-up sentinel (a silent allow)"
    );
}
