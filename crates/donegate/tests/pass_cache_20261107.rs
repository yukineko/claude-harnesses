// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Pass-cache contract for donegate's Stop checks (user request 2026-11-07,
//! "毎回のサイクルが重い"): a check that PASSED on a working tree must not be
//! re-executed while the tree (tracked + untracked-not-ignored content) and the
//! check command are unchanged.
//!
//! Observation: the check appends a line to a counter file OUTSIDE the tree;
//! the line count is the number of executions. Only exit codes, the
//! `decision` field and counts are asserted -- never prose (user ruling).
//!
//! `donegate.toml` is gitignored (committed `.gitignore`) so editing the check
//! command does not change the tree content: property 3 then isolates the
//! command-string axis from the tree axis.
//!
//! Properties (RED = fails at HEAD without a cache, CONTROL = passes at HEAD
//! and must keep passing after the cache lands):
//! - 1 (RED): unchanged tree + same cmd after a pass is not re-executed.
//! - 2a (CONTROL): editing a tracked file re-executes.
//! - 2b (CONTROL): adding an untracked file re-executes.
//! - 3 (CONTROL): a different cmd string on the same tree re-executes.
//! - 4 (CONTROL): a FAILED run is never cached; re-executed and still blocks.
//! - 5a (CONTROL): root is not a git repo (key uncomputable) executes.
//! - 5b (CONTROL): a corrupted cache store executes. Vacuous at HEAD where no
//!   store exists; meaningful after the cache lands.
//!
//! Written by a disinterested author (CLAUDE.md section 2(a)).
//! Do NOT weaken a test to make it pass; the `#[ignore]` marker on a RED test
//! is removed when the cache is implemented.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static N: AtomicU32 = AtomicU32::new(0);

struct Fx {
    home: PathBuf,
    root: PathBuf,
    counter: PathBuf,
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

impl Fx {
    /// `repo`: make the project a git repo. The check cmd counts executions.
    fn new(repo: bool) -> Fx {
        let n = N.fetch_add(1, Ordering::SeqCst);
        let home = std::env::temp_dir().join(format!("dg-pcache-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let root = home.join("project");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("seed.rs"), "fn seed() {}\n").unwrap();
        std::fs::write(root.join(".gitignore"), "donegate.toml\n").unwrap();
        if repo {
            git(&root, &["init", "-q"]);
            git(&root, &["config", "user.email", "t@t.com"]);
            git(&root, &["config", "user.name", "t"]);
            git(&root, &["add", "seed.rs", ".gitignore"]);
            git(
                &root,
                &["-c", "commit.gpgsign=false", "commit", "-qm", "init"],
            );
        }
        let root = root.canonicalize().unwrap();
        std::fs::create_dir_all(home.join(".harness")).unwrap();
        std::fs::write(
            home.join(".harness/trust.toml"),
            format!("trusted = [\"{}\"]\n", root.display()),
        )
        .unwrap();
        let counter = home.join("counter.txt");
        let fx = Fx {
            home,
            root,
            counter,
        };
        fx.set_cmd("exit 0");
        fx
    }

    /// The check always appends one line to the counter, then runs `tail`.
    fn set_cmd(&self, tail: &str) {
        std::fs::write(
            self.root.join("donegate.toml"),
            format!(
                "max_attempts = 50\nstate_dir = \"{}\"\n[[check]]\nname = \"chk\"\ncmd = \"echo x >> {}; {tail}\"\n",
                self.home.join("sd").display(),
                self.counter.display()
            ),
        )
        .unwrap();
    }

    fn runs(&self) -> usize {
        std::fs::read_to_string(&self.counter)
            .map(|s| s.lines().count())
            .unwrap_or(0)
    }

    /// One Stop hook invocation; returns (exit code, blocked?).
    fn stop(&self) -> (Option<i32>, bool) {
        let payload = serde_json::json!({
            "session_id": "s-pcache",
            "cwd": self.root.to_string_lossy(),
            "hook_event_name": "Stop",
        })
        .to_string();
        let mut child =
            harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_donegate")))
                .arg("gate")
                .current_dir(&self.root)
                .env("HOME", &self.home)
                .env("CLAUDE_CODE_SESSION_ID", "s-pcache")
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
            .unwrap()
            .write_all(payload.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        let blocked = stdout.lines().any(|l| {
            serde_json::from_str::<serde_json::Value>(l)
                .ok()
                .and_then(|v| v.get("decision").and_then(|d| d.as_str()).map(String::from))
                .as_deref()
                == Some("block")
        });
        (out.status.code(), blocked)
    }
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else {
                out.push(p);
            }
        }
    }
}

#[test]
#[ignore = "backlog e04711be: open defect, remove ignore when fixed"]
fn p1_unchanged_tree_and_cmd_after_a_pass_is_not_reexecuted() {
    let fx = Fx::new(true);
    assert_eq!(fx.stop(), (Some(0), false));
    assert_eq!(fx.runs(), 1);
    assert_eq!(fx.stop(), (Some(0), false), "verdict stays pass on a hit");
    assert_eq!(fx.runs(), 1, "check re-executed on an unchanged tree");
}

#[test]
fn p2a_editing_a_tracked_file_reexecutes() {
    let fx = Fx::new(true);
    assert_eq!(fx.stop(), (Some(0), false));
    std::fs::write(fx.root.join("seed.rs"), "fn seed() { /* edited */ }\n").unwrap();
    assert_eq!(fx.stop(), (Some(0), false));
    assert_eq!(fx.runs(), 2);
}

#[test]
fn p2b_adding_an_untracked_file_reexecutes() {
    let fx = Fx::new(true);
    assert_eq!(fx.stop(), (Some(0), false));
    std::fs::write(fx.root.join("new.rs"), "fn n() {}\n").unwrap();
    assert_eq!(fx.stop(), (Some(0), false));
    assert_eq!(fx.runs(), 2);
}

#[test]
fn p3_different_cmd_string_on_same_tree_reexecutes() {
    let fx = Fx::new(true);
    assert_eq!(fx.stop(), (Some(0), false));
    fx.set_cmd("true; exit 0");
    assert_eq!(fx.stop(), (Some(0), false));
    assert_eq!(fx.runs(), 2);
}

#[test]
fn p4_a_failed_run_is_never_cached() {
    let fx = Fx::new(true);
    // The output embeds the counter so each failure differs: an identical
    // failure would be waived by the repeat-ledger (not a block), which is a
    // different mechanism than the one under test.
    let tail = format!("cat {}; exit 1", fx.counter.display());
    fx.set_cmd(&tail);
    let (_, b1) = fx.stop();
    assert!(b1, "failing check must block");
    let (_, b2) = fx.stop();
    assert!(b2, "still blocks on the same tree");
    assert_eq!(fx.runs(), 2, "a failed run must be executed again");
}

#[test]
fn p5a_uncomputable_key_not_a_git_repo_executes_every_time() {
    let fx = Fx::new(false);
    assert_eq!(fx.stop(), (Some(0), false));
    assert_eq!(fx.stop(), (Some(0), false));
    assert_eq!(fx.runs(), 2, "no key => never a cache hit");
}

#[test]
fn p5b_corrupt_cache_store_executes_instead_of_hitting() {
    let fx = Fx::new(true);
    let mut before = Vec::new();
    walk(&fx.home, &mut before);
    assert_eq!(fx.stop(), (Some(0), false));
    let mut after = Vec::new();
    walk(&fx.home, &mut after);
    // Corrupt every file the first (passing) run created under HOME, except
    // the counter (our probe). The cache store, wherever under HOME it lives,
    // is among them once a cache exists.
    for p in after
        .iter()
        .filter(|p| !before.contains(p) && **p != fx.counter)
    {
        std::fs::write(p, b"\x00\xffnot json, not anything\x00").unwrap();
    }
    let (code, blocked) = fx.stop();
    assert!(
        !blocked && code == Some(0),
        "corrupt store must not block a green tree"
    );
    assert_eq!(fx.runs(), 2, "corrupt store must never be a hit");
}
