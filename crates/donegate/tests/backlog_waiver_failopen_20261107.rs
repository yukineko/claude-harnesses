// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Behavior pins for three claimed fail-open paths in donegate's identical-finding
//! waiver (backlog b80023b3, commit fb2b3eb6). The claims come from an independent
//! verifier's code reading; each test here observes the real binary. Only the
//! stdout decision / exit code / presence of a systemMessage is asserted — never
//! wording (user ruling: no tests on prose).
//!
//! F1 stale memory: block on tree S -> fix (green, allowed) -> revert to S (red):
//!    the stop is the FIRST block of a new chain and must block.
//! F2 a waived stop must not consume an attempt: max_attempts 2 (default),
//!    block, waived identical, then a CHANGED red tree must still block.
//! F3 manual CLI (empty stdin) twice on an unchanged red tree must exit 1 both
//!    times; and two hook Stops from different sessions lacking session_id must
//!    not waive each other.
//!
//! Written by a disinterested author (CLAUDE.md section 2(a)): no fix exists.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static N: AtomicU32 = AtomicU32::new(0);

struct Fixture {
    home: PathBuf,
    repo: PathBuf,
}

fn git(dir: &Path, args: &[&str]) {
    let st = harness_core::git_env::scrub(&mut Command::new("git"))
        .current_dir(dir)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("git runs");
    assert!(st.success(), "git {args:?} failed in {}", dir.display());
}

struct Stop {
    code: Option<i32>,
    decision: String,
    has_system_message: bool,
    stdout: String,
    stderr: String,
}

impl Stop {
    fn blocked(&self) -> bool {
        self.decision == "block"
    }
}

impl Fixture {
    /// A trusted repo whose only check is red unless `flag.txt` contains `ok`.
    /// `flag.txt` is untracked, so it is part of the tree fingerprint.
    fn new() -> Fixture {
        let n = N.fetch_add(1, Ordering::SeqCst);
        let home = std::env::temp_dir().join(format!("dg-waiver-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let repo = home.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        git(&repo, &["config", "user.email", "t@t.com"]);
        git(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("seed.rs"), "fn seed() {}\n").unwrap();
        std::fs::write(
            repo.join("donegate.toml"),
            "[[check]]\nname = \"flag-check\"\n\
             cmd = \"echo 'flag-check: red output'; grep -q '^ok$' flag.txt\"\n",
        )
        .unwrap();
        git(&repo, &["add", "seed.rs", "donegate.toml"]);
        git(
            &repo,
            &["-c", "commit.gpgsign=false", "commit", "-qm", "init"],
        );
        let repo = repo.canonicalize().unwrap();
        std::fs::create_dir_all(home.join(".harness")).unwrap();
        std::fs::write(
            home.join(".harness/trust.toml"),
            format!("trusted = [\"{}\"]\n", repo.display()),
        )
        .unwrap();
        let f = Fixture { home, repo };
        f.set_flag("bad-S");
        f
    }

    fn set_flag(&self, content: &str) {
        std::fs::write(self.repo.join("flag.txt"), format!("{content}\n")).unwrap();
    }

    /// `payload`: Some(json) pipes it to the hook; None pipes EMPTY stdin
    /// (manual CLI mode: HookInput::parse("") is None).
    fn run(&self, payload: Option<serde_json::Value>) -> Stop {
        let mut child =
            harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_donegate")))
                .arg("gate")
                .current_dir(&self.repo)
                .env("HOME", &self.home)
                .env_remove("CLAUDE_CODE_SESSION_ID")
                .env_remove("DONEGATE_DISABLE")
                .env_remove("HARNESS_TRUST_ALL")
                .env_remove("HARNESS_WORKTREE_ORIGIN_DIR")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("donegate spawns");
        let body = payload.map(|p| p.to_string()).unwrap_or_default();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(body.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let v = serde_json::from_str::<serde_json::Value>(stdout.trim()).ok();
        let decision = v
            .as_ref()
            .and_then(|v| v.get("decision"))
            .and_then(|d| d.as_str())
            .unwrap_or("")
            .to_string();
        let has_system_message = v
            .as_ref()
            .and_then(|v| v.get("systemMessage"))
            .and_then(|d| d.as_str())
            .is_some_and(|s| !s.is_empty());
        Stop {
            code: out.status.code(),
            decision,
            has_system_message,
            stdout,
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    fn hook(&self, session: Option<&str>, active: bool) -> Stop {
        let mut p = serde_json::json!({
            "cwd": self.repo.to_string_lossy(),
            "hook_event_name": "Stop",
            "stop_hook_active": active,
        });
        if let Some(s) = session {
            p["session_id"] = serde_json::json!(s);
        }
        self.run(Some(p))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn show(s: &Stop) -> String {
    format!(
        "code={:?} decision={:?} systemMessage_present={} stdout={:?} stderr={:?}",
        s.code, s.decision, s.has_system_message, s.stdout, s.stderr
    )
}

/// CONTROL (passes today): plain identical repeat is waived, non-silently.
#[test]
fn control_plain_identical_repeat_is_waived_with_system_message() {
    let f = Fixture::new();
    let first = f.hook(Some("s-ctl"), false);
    assert!(
        first.blocked(),
        "apparatus: first must block; {}",
        show(&first)
    );
    let second = f.hook(Some("s-ctl"), true);
    assert!(
        !second.blocked(),
        "identical repeat must be waived; {}",
        show(&second)
    );
    assert!(
        second.has_system_message,
        "a waiver must not be silent; {}",
        show(&second)
    );
}

/// CONTROL (passes today): different session ids never waive each other.
#[test]
fn control_different_named_sessions_do_not_waive_each_other() {
    let f = Fixture::new();
    let a = f.hook(Some("s-a"), false);
    assert!(a.blocked(), "apparatus: {}", show(&a));
    let b = f.hook(Some("s-b"), false);
    assert!(
        b.blocked(),
        "a different session is a first block; {}",
        show(&b)
    );
}

#[test]
#[ignore = "RED: b80023b3 waiver fail-open (verifier F1/F2/F3) 2026-11-07; remove when fixed"]
fn f1_revert_to_blocked_state_after_green_is_a_new_chain_and_blocks() {
    let f = Fixture::new();
    let s1 = f.hook(Some("s-f1"), false);
    assert!(s1.blocked(), "apparatus: stop 1 must block; {}", show(&s1));
    // The agent fixes it: the check goes green and the stop is allowed.
    f.set_flag("ok");
    let green = f.hook(Some("s-f1"), true);
    assert!(
        !green.blocked() && green.code == Some(0),
        "apparatus: the fixed tree must be allowed; {}",
        show(&green)
    );
    // Revert byte-for-byte to S: red again.
    f.set_flag("bad-S");
    let again = f.hook(Some("s-f1"), false);
    assert!(
        again.blocked(),
        "F1: after an intervening green the revert to the blocked state S is the FIRST \
         block of a new chain and must block, but the stale signature waived it; {}",
        show(&again)
    );
}

#[test]
#[ignore = "RED: b80023b3 waiver fail-open (verifier F1/F2/F3) 2026-11-07; remove when fixed"]
fn f2_waived_stop_does_not_consume_an_attempt_changed_tree_still_blocks() {
    let f = Fixture::new(); // default max_attempts (2), reset_after_secs 600
    let s1 = f.hook(Some("s-f2"), false);
    assert!(s1.blocked(), "apparatus: stop 1 must block; {}", show(&s1));
    let s2 = f.hook(Some("s-f2"), true);
    assert!(
        !s2.blocked(),
        "apparatus: stop 2 identical must be waived; {}",
        show(&s2)
    );
    // The tree CHANGES (still red, different content).
    f.set_flag("bad-T-different");
    let s3 = f.hook(Some("s-f2"), true);
    assert!(
        s3.blocked(),
        "F2: a changed red tree must still block; the waived stop is not a block and \
         must not count toward the 2 consecutive blocks, but stop 3 was an immediate \
         give-up allow; {}",
        show(&s3)
    );
}

#[test]
#[ignore = "RED: b80023b3 waiver fail-open (verifier F1/F2/F3) 2026-11-07; remove when fixed"]
fn f3a_manual_cli_twice_on_unchanged_red_tree_exits_1_both_times() {
    let f = Fixture::new();
    let first = f.run(None);
    assert_eq!(
        first.code,
        Some(1),
        "apparatus: manual run 1 must exit 1; {}",
        show(&first)
    );
    let second = f.run(None);
    assert_eq!(
        second.code,
        Some(1),
        "F3a: manual mode reports failure by exit code; a repeat on an unchanged red \
         tree must still exit 1, but it was waived; {}",
        show(&second)
    );
}

#[test]
#[ignore = "RED: b80023b3 waiver fail-open (verifier F1/F2/F3) 2026-11-07; remove when fixed"]
fn f3b_hook_stops_from_different_sessions_without_session_id_do_not_waive_each_other() {
    let f = Fixture::new();
    let a = f.hook(None, false);
    assert!(
        a.blocked(),
        "apparatus: first stop must block; {}",
        show(&a)
    );
    // A different session (its payload also lacks session_id; fresh chain, no
    // stop_hook_active). A shared "_local" bucket must not let A's block waive B.
    let b = f.hook(None, false);
    assert!(
        b.blocked(),
        "F3b: a missing session_id cannot be shared safely; the second stop must block, \
         but it was waived; {}",
        show(&b)
    );
}
