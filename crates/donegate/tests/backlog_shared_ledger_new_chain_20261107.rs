// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Behavior pin for an unverified verifier claim (condukt run-20261107-051454-39899):
//! "With CLAUDE_CODE_SESSION_ID set, the shared `harness_core::repeat` ledger (operator
//! ruling 2026-09-18) can still waive the FIRST stop of a NEW block chain in donegate
//! when the block text repeats."
//!
//! donegate's own identical-finding memory (`state::is_repeat`) is chain-scoped, but
//! the block is also routed through `repeat::emit_stop_block_keyed`, keyed on
//! `gate::block_fingerprint` (which keeps `attempt N/max`), and that ledger lives under
//! `$HOME/.claude/harness-state/repeat/<CLAUDE_CODE_SESSION_ID>/` and is never cleared
//! when a chain ends. The existing `backlog_waiver_failopen_20261107.rs::f1_*` removes
//! CLAUDE_CODE_SESSION_ID, so the shared ledger is Undetermined there and never waives;
//! this file sets it.
//!
//! Only the stdout decision and the exit code are asserted (no prose).
//! Written by a disinterested author (CLAUDE.md section 2(a)): no fix exists.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static N: AtomicU32 = AtomicU32::new(0);

const ENV_SESSION: &str = "env-sess-shared-ledger";

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
    stdout: String,
    stderr: String,
}

impl Stop {
    fn blocked(&self) -> bool {
        self.decision == "block"
    }
}

fn show(s: &Stop) -> String {
    format!(
        "code={:?} decision={:?} stdout={:?} stderr={:?}",
        s.code, s.decision, s.stdout, s.stderr
    )
}

impl Fixture {
    /// A trusted repo whose only check is red unless `flag.txt` is `ok`. The check's
    /// output is constant, so every red run renders the same block text; `flag.txt`
    /// is untracked, so changing it changes the tree fingerprint (and with it
    /// donegate's own chain-scoped signature) without changing the output.
    fn new() -> Fixture {
        let n = N.fetch_add(1, Ordering::SeqCst);
        let home = std::env::temp_dir().join(format!("dg-shledger-{}-{n}", std::process::id()));
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

    /// The shared repeat ledger for ENV_SESSION (what `repeat::default_state_dir`
    /// resolves to under this HOME).
    fn shared_ledger(&self) -> PathBuf {
        self.home
            .join(".claude/harness-state/repeat")
            .join(ENV_SESSION)
    }

    fn hook(&self, session: &str, active: bool) -> Stop {
        let payload = serde_json::json!({
            "cwd": self.repo.to_string_lossy(),
            "hook_event_name": "Stop",
            "stop_hook_active": active,
            "session_id": session,
        });
        let mut child =
            harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_donegate")))
                .arg("gate")
                .current_dir(&self.repo)
                .env("HOME", &self.home)
                .env("CLAUDE_CODE_SESSION_ID", ENV_SESSION)
                .env_remove("DONEGATE_DISABLE")
                .env_remove("HARNESS_TRUST_ALL")
                .env_remove("HARNESS_WORKTREE_ORIGIN_DIR")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("donegate spawns");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.to_string().as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let decision = serde_json::from_str::<serde_json::Value>(stdout.trim())
            .ok()
            .and_then(|v| v.get("decision").and_then(|d| d.as_str()).map(String::from))
            .unwrap_or_default();
        Stop {
            code: out.status.code(),
            decision,
            stdout,
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    /// Chain 1 ended by GREEN: block on red, then fix (allowed). Leaves the check
    /// red again (flag reverted) for the next chain's first stop.
    fn chain_ended_by_green(&self, session: &str) {
        let s1 = self.hook(session, false);
        assert!(
            s1.blocked() && s1.code == Some(0),
            "apparatus: chain-1 stop must block; {}",
            show(&s1)
        );
        self.set_flag("ok");
        let g = self.hook(session, true);
        assert!(
            !g.blocked() && g.code == Some(0),
            "apparatus: green stop must be allowed; {}",
            show(&g)
        );
        self.set_flag("bad-S");
    }

    /// Chain 1 ended by GIVE-UP (default max_attempts = 2): two blocks on changed
    /// red trees (so donegate's own identical-tree waiver cannot fire), the third
    /// red stop gives up and is allowed.
    fn chain_ended_by_giveup(&self, session: &str) {
        for (i, flag) in ["bad-S", "bad-T"].iter().enumerate() {
            self.set_flag(flag);
            let s = self.hook(session, i > 0);
            assert!(
                s.blocked() && s.code == Some(0),
                "apparatus: chain-1 stop {} must block; {}",
                i + 1,
                show(&s)
            );
        }
        self.set_flag("bad-U");
        let gu = self.hook(session, true);
        assert!(
            !gu.blocked() && gu.code == Some(0),
            "apparatus: third red stop must be the give-up allow; {}",
            show(&gu)
        );
        self.set_flag("bad-S");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn assert_new_chain_first_stop_blocks(f: &Fixture, session: &str, how: &str) {
    let first = f.hook(session, false);
    assert!(
        first.blocked() && first.code == Some(0),
        "{how}: the FIRST stop of a new chain with a red check must block \
         (decision=block, exit 0); {}",
        show(&first)
    );
}

/// CONTROL: a fresh session with no prior ledger entry blocks its first red stop.
#[test]
fn control_fresh_first_stop_blocks() {
    let f = Fixture::new();
    assert_new_chain_first_stop_blocks(&f, "s-ctl-fresh", "fresh");
}

/// CONTROL: chain ended by green, shared ledger removed before the new chain -> blocks.
/// Differs from the green-RED test only in the absence of the prior ledger entry.
#[test]
fn control_after_green_without_prior_ledger_entry_blocks() {
    let f = Fixture::new();
    f.chain_ended_by_green("s-ctl-green");
    assert!(
        f.shared_ledger().is_dir(),
        "apparatus: chain 1 must have written the shared ledger"
    );
    std::fs::remove_dir_all(f.shared_ledger()).unwrap();
    assert_new_chain_first_stop_blocks(&f, "s-ctl-green", "control(green, no ledger)");
}

/// CONTROL: chain ended by give-up, shared ledger removed before the new chain -> blocks.
#[test]
fn control_after_giveup_without_prior_ledger_entry_blocks() {
    let f = Fixture::new();
    f.chain_ended_by_giveup("s-ctl-giveup");
    assert!(
        f.shared_ledger().is_dir(),
        "apparatus: chain 1 must have written the shared ledger"
    );
    std::fs::remove_dir_all(f.shared_ledger()).unwrap();
    assert_new_chain_first_stop_blocks(&f, "s-ctl-giveup", "control(give-up, no ledger)");
}

#[test]
#[ignore = "RED 2026-11-07: shared repeat ledger waives first stop of a new chain; remove ignore when fixed"]
fn new_chain_after_green_first_stop_blocks_despite_shared_ledger() {
    let f = Fixture::new();
    f.chain_ended_by_green("s-green");
    assert_new_chain_first_stop_blocks(&f, "s-green", "after green");
}

#[test]
#[ignore = "RED 2026-11-07: shared repeat ledger waives first stop of a new chain; remove ignore when fixed"]
fn new_chain_after_giveup_first_stop_blocks_despite_shared_ledger() {
    let f = Fixture::new();
    f.chain_ended_by_giveup("s-giveup");
    assert_new_chain_first_stop_blocks(&f, "s-giveup", "after give-up");
}
