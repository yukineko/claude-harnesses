#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Pins the `rest.contains("..")` refusal of `literal_cd_and_rest`
//! (`src/detect.rs`).
//!
//! For a line `cd D && REST` that `literal_cd_and_rest` accepts, `Ctx::new`
//! also evaluates the eligibility predicates on `REST` alone. Of the three,
//! only `checkout_exempt_eligible` reads no `..` rule of its own (the two rm
//! predicates refuse any `..` in an `rm` word), so without the `..` refusal a
//! `REST` such as `rm ../.githooks/pre-push` makes the line checkout-exempt.
//! The relative operand is then placed at `D/../.githooks/pre-push`, which
//! the scope collapses LEXICALLY before resolving symlinks. With `D` a symlink
//! inside a linked worktree that points into the MAIN tree, the lexical
//! collapse lands inside the worktree (`<wt>/.githooks/pre-push`, Allow), but
//! the kernel resolves `..` against the symlink's REAL parent and deletes the
//! main tree's own gate `<proj>/.githooks/pre-push`, which lies outside every
//! safe root.
//!
//! Every negative case is paired with an anti-vacuity CONTROL in the same
//! fixture: `cd <wt>/.githooks && rm pre-push` (no `..`) must be Allow, which
//! proves the tied-`REST` relaxation is live here, so a not-Allow verdict on
//! the `..` spelling is attributable to the `..` refusal and not to a fixture
//! that never reaches the checkout exemption at all.
//!
//! Each call uses its own session id: the repeat-downgrade ledger would turn
//! a second identical Deny into Ask (still not Allow, but it would blur what
//! the assertions observe).

#[path = "support/git_env.rs"]
mod git_env;
#[path = "support/neutral_base.rs"]
mod neutral_base;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, PartialEq, Eq)]
enum V {
    Allow,
    Ask,
    Deny,
}

fn git(dir: &Path, args: &[&str]) {
    let out = git_env::pinned(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn write(p: &Path, content: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}

struct Fx {
    base: PathBuf,
    home: PathBuf,
    /// Main tree `<base>/src/harness`, a git repo with `.githooks/pre-push`
    /// and a real directory `sub/`.
    proj: PathBuf,
    /// `<base>/src/.harness-worktrees/wt1`, a linked worktree of `proj`, with
    /// `lnk` a symlink to `<proj>/sub`.
    wt: PathBuf,
}

impl Fx {
    fn new(name: &str) -> Fx {
        let nonce = SEQ.fetch_add(1, Ordering::SeqCst);
        let base = neutral_base::neutral_base()
            .join("cd_rest_dotdot_guard")
            .join(format!("{name}-{}-{nonce}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let home = base.join("home");
        let proj = base.join("src/harness");
        let hw = base.join("src/.harness-worktrees");
        let wt = hw.join("wt1");

        write(&home.join(".claude/settings.json"), "{}\n");

        std::fs::create_dir_all(&proj).unwrap();
        git(&proj, &["init", "-q"]);
        write(&proj.join(".githooks/pre-push"), "#!/bin/sh\nexit 0\n");
        write(&proj.join("sub/keep.txt"), "x\n");
        git(&proj, &["add", "-A"]);
        git(&proj, &["commit", "-q", "-m", "init"]);

        std::fs::create_dir_all(&hw).unwrap();
        git(
            &proj,
            &["worktree", "add", "-q", "-b", "wt1", wt.to_str().unwrap()],
        );
        std::os::unix::fs::symlink(proj.join("sub"), wt.join("lnk")).unwrap();

        Fx {
            base,
            home,
            proj,
            wt,
        }
    }

    /// Verdict for one Bash call from a session in the main tree, under a
    /// fresh session id.
    fn run(&self, command: &str) -> V {
        let session = format!(
            "cd-rest-dotdot-{}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let payload = serde_json::json!({
            "session_id": session,
            "hook_event_name": "PreToolUse",
            "cwd": self.proj.display().to_string(),
            "tool_name": "Bash",
            "tool_input": { "command": command },
        })
        .to_string();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
        git_env::scrub(&mut cmd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("CLAUDE_PROJECT_DIR", &self.proj)
            .env("BLASTGUARD_APPROVALS_DIR", self.base.join("store"))
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_ENTRYPOINT", "cli")
            .env_remove("BLASTGUARD_ASK")
            .env_remove("TMPDIR")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .current_dir(&self.base);
        let mut child = cmd.spawn().expect("binary spawns");
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(payload.as_bytes());
        }
        let out = child.wait_with_output().expect("binary runs");
        assert_eq!(
            out.status.code(),
            Some(0),
            "hook must exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let s = String::from_utf8_lossy(&out.stdout).into_owned();
        println!("[{command}] => {:?}", s.trim());
        classify(&s)
    }
}

fn classify(s: &str) -> V {
    if s.trim().is_empty() {
        return V::Allow;
    }
    let v: serde_json::Value = serde_json::from_str(s.trim())
        .unwrap_or_else(|e| panic!("unparseable hook output {s:?}: {e}"));
    match v["hookSpecificOutput"]["permissionDecision"].as_str() {
        Some("allow") => V::Allow,
        Some("ask") => V::Ask,
        Some("deny") => V::Deny,
        other => panic!("unknown permissionDecision {other:?} in {s}"),
    }
}

fn d(p: &Path) -> String {
    p.display().to_string()
}

/// Anti-vacuity: the tied-`REST` checkout exemption is live in this fixture.
#[track_caller]
fn control(f: &Fx) {
    let c = format!("cd {}/.githooks && rm pre-push", d(&f.wt));
    assert_eq!(
        f.run(&c),
        V::Allow,
        "CONTROL {c}: the tied-REST checkout exemption must be live (Allow)"
    );
}

#[track_caller]
fn not_allow(case: &str, got: V) {
    assert_ne!(
        got,
        V::Allow,
        "{case}: `..` in REST must withhold the tied-REST relaxation (got Allow)"
    );
}

/// `cd <wt>/lnk && rm ../.githooks/pre-push` with `lnk -> <proj>/sub`: judged
/// lexically as `<wt>/.githooks/pre-push`, but the kernel deletes
/// `<proj>/.githooks/pre-push`, the main tree's gate.
#[test]
fn cd_symlink_into_main_tree_then_rm_dotdot_is_not_allow() {
    let f = Fx::new("rm_lnk");
    control(&f);
    let c = format!("cd {}/lnk && rm ../.githooks/pre-push", d(&f.wt));
    not_allow(&c, f.run(&c));
}

/// Same escape through a redirect target, which `checkout_exempt_eligible`
/// also accepts (`>` is not in its refused set).
#[test]
fn cd_symlink_into_main_tree_then_redirect_dotdot_is_not_allow() {
    let f = Fx::new("redir_lnk");
    control(&f);
    let c = format!("cd {}/lnk && echo exit > ../.githooks/pre-push", d(&f.wt));
    not_allow(&c, f.run(&c));
}

/// `chmod` through the same symlink escape.
#[test]
fn cd_symlink_into_main_tree_then_chmod_dotdot_is_not_allow() {
    let f = Fx::new("chmod_lnk");
    control(&f);
    let c = format!("cd {}/lnk && chmod 644 ../.githooks/pre-push", d(&f.wt));
    not_allow(&c, f.run(&c));
}
