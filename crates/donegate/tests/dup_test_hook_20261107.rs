// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Duplicate-`cargo test` hook contract (user decision 2026-11-07, "どれも実施する"):
//! agents re-run the same `cargo test` on an unchanged tree (measured ~3,300s of
//! exact duplicates plus up to ~7,300s of verifier re-runs). The hook pair
//! mechanizes "run targeted tests once".
//!
//! # Surface the implementer must match (chosen by the test writer)
//!
//! * `donegate dup-test pre`  -- PreToolUse(Bash) hook. Hook JSON on stdin.
//!   Exit 0 always. Deny = stdout JSON
//!   `{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny",
//!   "permissionDecisionReason":"..."}}`. Allow = NO `permissionDecision`
//!   "deny" anywhere (silent stdout is the expected form).
//!   It must also snapshot the pre-call tree key, keyed by `tool_use_id`, so
//!   `post` can prove the tree did not change DURING the call.
//! * `donegate dup-test post` -- PostToolUse(Bash) hook. Hook JSON on stdin,
//!   exit 0, no stdout decision. Records a pass ONLY when all hold:
//!   `hook_event_name == "PostToolUse"` (Claude Code sends a failing / interrupted
//!   Bash as `PostToolUseFailure` instead -- observed 2026-10-03, see
//!   scripts/guard-maintree-bash.py -- because the Bash `tool_response` carries
//!   NO exit code: only `stdout`, `stderr`, `interrupted`), `tool_response`
//!   exists with `interrupted == false`, the command is a plain single
//!   `cargo test ...` (no `&&` `;` `|` subshell, no `cd`), cwd is inside a git
//!   repo, and the tree key at post time == the key the matching pre (same
//!   `tool_use_id`) snapshotted. Key = git toplevel + exact normalized
//!   argv + working-tree content tree (passcache.rs `git add -A` into a temp
//!   GIT_INDEX_FILE + `write-tree`).
//! * Store: anywhere under `$HOME` (tests set HOME to a temp dir). The corrupt
//!   store control overwrites every file created under HOME by the first post.
//!
//! Payload fields used: `session_id`, `cwd`, `hook_event_name`, `tool_name`
//! (`Bash`), `tool_input.command`, `tool_use_id`, `tool_response`.
//!
//! Only exit codes and JSON `permissionDecision` are asserted -- never prose.
//!
//! # Status at authoring (HEAD cc99e57e): the `dup-test` subcommand does not
//! exist, so clap exits 2 for every call. Therefore EVERY test here fails at
//! HEAD, including the "controls" (they assert exit 0 + no deny) -- the harness
//! is not independently proven by a passing control. All are `#[ignore]`d under
//! the backlog id. Once implemented:
//! * RED (behavioral): `dup_same_cmd_same_tree_after_success_is_denied`.
//! * Controls (must ALLOW and pass once the surface exists): every other test.
//!   `c_corrupt_store_allows` additionally asserts the store file exists, so it
//!   is not vacuous.
//!
//! Written by a disinterested author (CLAUDE.md section 2(a)). Do NOT weaken a
//! test to make it pass; remove the ignore marker when fixed.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static N: AtomicU32 = AtomicU32::new(0);

const CARGO: &str = "cargo test -p foo --test bar";

struct Fx {
    home: PathBuf,
    root: PathBuf,
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

impl Fx {
    fn new(repo: bool) -> Fx {
        let n = N.fetch_add(1, Ordering::SeqCst);
        let home = std::env::temp_dir().join(format!("dg-duptest-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let root = home.join("project");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("seed.rs"), "fn seed() {}\n").unwrap();
        if repo {
            git(&root, &["init", "-q"]);
            git(&root, &["config", "user.email", "t@t.com"]);
            git(&root, &["config", "user.name", "t"]);
            git(&root, &["add", "seed.rs"]);
            git(
                &root,
                &["-c", "commit.gpgsign=false", "commit", "-qm", "init"],
            );
        }
        let root = root.canonicalize().unwrap();
        Fx { home, root }
    }

    /// Run `donegate dup-test <sub>` with `payload`; (exit code, stdout).
    fn hook(&self, sub: &str, payload: &serde_json::Value) -> (Option<i32>, String) {
        let mut child =
            harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_donegate")))
                .args(["dup-test", sub])
                .current_dir(&self.root)
                .env("HOME", &self.home)
                .env("CLAUDE_CODE_SESSION_ID", "s-dup")
                .env_remove("DONEGATE_DISABLE")
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
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    }

    fn pre_payload(&self, cmd: &str, id: &str) -> serde_json::Value {
        serde_json::json!({
            "session_id": "s-dup",
            "cwd": self.root.to_string_lossy(),
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_input": {"command": cmd},
            "tool_use_id": id,
        })
    }

    fn post_payload(&self, cmd: &str, id: &str) -> serde_json::Value {
        serde_json::json!({
            "session_id": "s-dup",
            "cwd": self.root.to_string_lossy(),
            "hook_event_name": "PostToolUse",
            "tool_name": "Bash",
            "tool_input": {"command": cmd},
            "tool_use_id": id,
            "tool_response": {"stdout": "ok", "stderr": "", "interrupted": false},
        })
    }

    /// True iff the pre hook DENIES this command (exit must be 0 either way).
    fn pre_denies(&self, cmd: &str, id: &str) -> bool {
        let (code, out) = self.hook("pre", &self.pre_payload(cmd, id));
        assert_eq!(code, Some(0), "pre hook must exit 0; stdout={out}");
        out.lines().any(|l| {
            serde_json::from_str::<serde_json::Value>(l)
                .ok()
                .and_then(|v| {
                    v.pointer("/hookSpecificOutput/permissionDecision")
                        .and_then(|d| d.as_str())
                        .map(String::from)
                })
                .as_deref()
                == Some("deny")
        })
    }

    fn post(&self, payload: &serde_json::Value) {
        let (code, out) = self.hook("post", payload);
        assert_eq!(code, Some(0), "post hook must exit 0; stdout={out}");
    }

    /// A full successful call: pre (must allow), then post(success).
    fn run_ok(&self, cmd: &str, id: &str) {
        assert!(!self.pre_denies(cmd, id), "first run must be allowed");
        self.post(&self.post_payload(cmd, id));
    }

    fn files_outside_project(&self) -> Vec<PathBuf> {
        let mut v = Vec::new();
        walk(&self.home, &mut v);
        v.retain(|p| !p.starts_with(&self.root));
        v
    }
}

#[test]
#[ignore = "backlog PENDING: donegate dup-test hook not implemented"]
fn dup_same_cmd_same_tree_after_success_is_denied() {
    let fx = Fx::new(true);
    fx.run_ok(CARGO, "t1");
    assert!(
        fx.pre_denies(CARGO, "t2"),
        "same argv on a byte-identical tree after a success must be denied"
    );
}

#[test]
#[ignore = "backlog PENDING: donegate dup-test hook not implemented"]
fn c_tracked_edit_allows() {
    let fx = Fx::new(true);
    fx.run_ok(CARGO, "t1");
    std::fs::write(fx.root.join("seed.rs"), "fn seed() { /* edited */ }\n").unwrap();
    assert!(!fx.pre_denies(CARGO, "t2"));
}

#[test]
#[ignore = "backlog PENDING: donegate dup-test hook not implemented"]
fn c_untracked_file_allows() {
    let fx = Fx::new(true);
    fx.run_ok(CARGO, "t1");
    std::fs::write(fx.root.join("new.rs"), "fn n() {}\n").unwrap();
    assert!(!fx.pre_denies(CARGO, "t2"));
}

#[test]
#[ignore = "backlog PENDING: donegate dup-test hook not implemented"]
fn c_extra_filter_allows() {
    let fx = Fx::new(true);
    fx.run_ok(CARGO, "t1");
    assert!(!fx.pre_denies("cargo test -p foo --test bar some_filter", "t2"));
}

#[test]
#[ignore = "backlog PENDING: donegate dup-test hook not implemented"]
fn c_include_ignored_allows() {
    let fx = Fx::new(true);
    fx.run_ok(CARGO, "t1");
    assert!(!fx.pre_denies("cargo test -p foo --test bar -- --include-ignored", "t2"));
}

#[test]
#[ignore = "backlog PENDING: donegate dup-test hook not implemented"]
fn c_failed_run_is_not_recorded() {
    let fx = Fx::new(true);
    assert!(!fx.pre_denies(CARGO, "t1"));
    let mut p = fx.post_payload(CARGO, "t1");
    p["hook_event_name"] = serde_json::json!("PostToolUseFailure");
    fx.post(&p);
    assert!(!fx.pre_denies(CARGO, "t2"));
}

#[test]
#[ignore = "backlog PENDING: donegate dup-test hook not implemented"]
fn c_interrupted_run_is_not_recorded() {
    let fx = Fx::new(true);
    assert!(!fx.pre_denies(CARGO, "t1"));
    let mut p = fx.post_payload(CARGO, "t1");
    p["tool_response"]["interrupted"] = serde_json::json!(true);
    fx.post(&p);
    assert!(!fx.pre_denies(CARGO, "t2"));
}

#[test]
#[ignore = "backlog PENDING: donegate dup-test hook not implemented"]
fn c_undeterminable_success_is_not_recorded() {
    // No hook_event_name, and separately no tool_response / no `interrupted`.
    let fx = Fx::new(true);
    let mutations: [fn(&mut serde_json::Value); 3] = [
        |p| {
            p.as_object_mut().unwrap().remove("hook_event_name");
        },
        |p| {
            p.as_object_mut().unwrap().remove("tool_response");
        },
        |p| {
            p["tool_response"] = serde_json::json!({"stdout": "ok", "stderr": ""});
        },
    ];
    for (i, mutate) in mutations.iter().enumerate() {
        let id = format!("u{i}");
        assert!(!fx.pre_denies(CARGO, &id));
        let mut p = fx.post_payload(CARGO, &id);
        mutate(&mut p);
        fx.post(&p);
        assert!(
            !fx.pre_denies(CARGO, &format!("v{i}")),
            "variant {i}: success not determinable => nothing recorded"
        );
    }
}

#[test]
#[ignore = "backlog PENDING: donegate dup-test hook not implemented"]
fn c_tree_changed_during_the_call_is_not_recorded() {
    let fx = Fx::new(true);
    assert!(!fx.pre_denies(CARGO, "t1"));
    std::fs::write(fx.root.join("seed.rs"), "fn seed() { /* mid-call */ }\n").unwrap();
    fx.post(&fx.post_payload(CARGO, "t1"));
    // Tree now equals the post-call tree; the pre/post keys differed, so no pass.
    assert!(!fx.pre_denies(CARGO, "t2"));
}

#[test]
#[ignore = "backlog PENDING: donegate dup-test hook not implemented"]
fn c_compound_commands_are_never_recorded_nor_denied() {
    let fx = Fx::new(true);
    for (i, cmd) in [
        "cargo test -p foo --test bar && echo done",
        "cargo test -p foo --test bar; true",
        "cargo test -p foo --test bar | tail -5",
        "(cargo test -p foo --test bar)",
        "cd /tmp && cargo test -p foo --test bar",
    ]
    .iter()
    .enumerate()
    {
        let id = format!("c{i}");
        assert!(!fx.pre_denies(cmd, &id));
        fx.post(&fx.post_payload(cmd, &id));
        assert!(!fx.pre_denies(cmd, &format!("d{i}")), "compound: {cmd}");
        // Nor may the compound run have recorded the plain command's key.
        assert!(
            !fx.pre_denies(CARGO, &format!("e{i}")),
            "plain after: {cmd}"
        );
    }
}

#[test]
#[ignore = "backlog PENDING: donegate dup-test hook not implemented"]
fn c_not_a_git_repo_allows() {
    let fx = Fx::new(false);
    fx.run_ok(CARGO, "t1");
    assert!(!fx.pre_denies(CARGO, "t2"));
}

#[test]
#[ignore = "backlog PENDING: donegate dup-test hook not implemented"]
fn c_corrupt_store_allows() {
    let fx = Fx::new(true);
    let before = fx.files_outside_project();
    fx.run_ok(CARGO, "t1");
    let created: Vec<PathBuf> = fx
        .files_outside_project()
        .into_iter()
        .filter(|p| !before.contains(p))
        .collect();
    assert!(
        !created.is_empty(),
        "a recorded pass must leave a store file under HOME (else this control is vacuous)"
    );
    for p in &created {
        std::fs::write(p, b"\x00\xffnot json, not anything\x00").unwrap();
    }
    assert!(!fx.pre_denies(CARGO, "t2"), "corrupt store must never deny");
}

#[test]
#[ignore = "backlog PENDING: donegate dup-test hook not implemented"]
fn c_not_cargo_test_allows() {
    let fx = Fx::new(true);
    fx.run_ok("cargo build -p foo", "t1");
    assert!(!fx.pre_denies("cargo build -p foo", "t2"));
    // A passed cargo test must not make a different command denied.
    fx.run_ok(CARGO, "t3");
    assert!(!fx.pre_denies("cargo build -p foo", "t4"));
}
