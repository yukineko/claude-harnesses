#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Fail-open RED (user ruling 2026-11-20): git discard operations aimed at
//! ANOTHER session's linked worktree are Allow today even when that worktree
//! holds uncommitted changes.
//!
//! Written by an independent test author (not the implementer; CLAUDE.md
//! §2(a)). Ruling:
//!   * other session's linked worktree WITH uncommitted changes (a modified
//!     tracked file; for `git clean`, an untracked file) -> Ask in the main
//!     session, Deny in a subagent. The REDs below assert "main is not allow"
//!     (ask or deny) and "subagent is deny".
//!   * the own worktree (session cwd inside it) keeps today's verdict
//!     (control, measured and pinned).
//!   * a CLEAN other worktree: pinned as measured today (control).
//!
//! Ops x forms covered by the REDs:
//!   ops   = `reset --hard`, `checkout .`, `restore m.txt`, `restore .`,
//!           `clean -fd`
//!   forms = `git -C <wt> <op>` and `cd <wt> && git <op>`
//!   session layouts = cwd in the main tree (`<base>/src/proj`) and cwd in the
//!           session's own linked worktree (`.../wt-own`)
//!
//! Fixture (hermetic, same location rule as
//! `s6_tablec_characterize_20261120.rs`):
//!   * `<base>/home`                              — the hook's `HOME`
//!   * `<base>/src/proj`                          — git repo, main tree;
//!     `CLAUDE_PROJECT_DIR` in both layouts
//!   * `<base>/src/.harness-worktrees/wt-own`     — linked worktree; the
//!     session cwd in the `OwnWt` layout
//!   * `<base>/src/.harness-worktrees/wt-other`   — linked worktree that does
//!     NOT hold the session cwd (another session's)
//!
//! Each checkout tracks `t.txt`, `m.txt`. Dirt kinds: `Modified` (m.txt
//! changed, nothing untracked), `Untracked` (u.txt untracked, nothing
//! modified), `Clean`. The hook runs with `env_clear()` plus `HOME`,
//! `PATH=/usr/bin:/bin`, `CLAUDECODE=1`, `CLAUDE_CODE_ENTRYPOINT=cli`,
//! `CLAUDE_PROJECT_DIR`, `BLASTGUARD_APPROVALS_DIR`; no `TMPDIR`, no
//! `CLAUDE_CODE_SESSION_ID`. Subagent mode = `agent_id` in the payload.
//! Nothing is executed; the hook only judges the text.
//!
//! DOES NOT PROVE: how the fix tells "own" from "other" (only the decision is
//! asserted); verdicts with a session id or `TMPDIR` set; `--work-tree`
//! spellings; relative `-C` spellings; staged-only changes; worktrees outside
//! the storage root; that ops not listed here are covered.

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/git_env.rs"]
mod git_env;
#[path = "support/neutral_base.rs"]
mod neutral_base;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Fx {
    base: PathBuf,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Dirt {
    Clean,
    Modified,
    Untracked,
}

#[derive(Clone, Copy, Debug)]
enum Layout {
    /// Session cwd = main tree.
    MainCwd,
    /// Session cwd = the session's own linked worktree.
    OwnWt,
}

#[derive(Clone, Copy, Debug)]
enum Target {
    OtherWt,
    OwnWt,
}

impl Fx {
    fn home(&self) -> PathBuf {
        self.base.join("home")
    }
    fn repo(&self) -> PathBuf {
        self.base.join("src").join("proj")
    }
    fn wt(&self, name: &str) -> PathBuf {
        self.base.join("src").join(".harness-worktrees").join(name)
    }
    fn cwd(&self, l: Layout) -> PathBuf {
        match l {
            Layout::MainCwd => self.repo(),
            Layout::OwnWt => self.wt("wt-own"),
        }
    }
    fn target(&self, t: Target) -> PathBuf {
        match t {
            Target::OtherWt => self.wt("wt-other"),
            Target::OwnWt => self.wt("wt-own"),
        }
    }

    fn git(&self, dir: &Path, args: &[&str]) {
        let mut cmd = git_env::pinned(dir);
        cmd.args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", self.home());
        let out =
            bounded::run_bounded_stdin(&mut cmd, bounded::ChildStdin::Null, bounded::TOOL_DEADLINE);
        assert!(
            out.status.success(),
            "fixture git {args:?} in {} failed: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// `git status --porcelain` of `dir`, to prove the fixture's dirt kind.
    fn status(&self, dir: &Path) -> String {
        let mut cmd = git_env::pinned(dir);
        cmd.args(["status", "--porcelain", "--untracked-files=all"])
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", self.home());
        let out =
            bounded::run_bounded_stdin(&mut cmd, bounded::ChildStdin::Null, bounded::TOOL_DEADLINE);
        assert!(out.status.success(), "fixture git status failed");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

fn write(p: &Path, body: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

fn make_dirty(root: &Path, d: Dirt) {
    match d {
        Dirt::Clean => {}
        Dirt::Modified => write(&root.join("m.txt"), "changed\n"),
        Dirt::Untracked => write(&root.join("u.txt"), "a\n"),
    }
}

/// Both linked worktrees get the same dirt kind.
fn fixture(tag: &str, d: Dirt) -> Fx {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let base = neutral_base::neutral_base().join(format!(
        "otherwt-git-{tag}-{}",
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("home").join(".cache")).unwrap();
    std::fs::create_dir_all(base.join("src").join("proj")).unwrap();
    std::fs::create_dir_all(base.join("src").join(".harness-worktrees")).unwrap();
    std::fs::create_dir_all(base.join("store")).unwrap();
    let fx = Fx { base };
    let repo = fx.repo();
    write(&repo.join("t.txt"), "a\n");
    write(&repo.join("m.txt"), "a\n");
    fx.git(&repo, &["init", "-q", "-b", "main"]);
    fx.git(&repo, &["add", "-A"]);
    fx.git(&repo, &["commit", "-q", "-m", "init"]);
    for (name, br) in [("wt-own", "own"), ("wt-other", "other")] {
        let wt = fx.wt(name);
        fx.git(
            &repo,
            &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", br],
        );
        make_dirty(&wt, d);
        let st = fx.status(&wt);
        match d {
            Dirt::Clean => assert!(st.is_empty(), "{name} not clean: {st:?}"),
            Dirt::Modified => assert_eq!(st, " M m.txt\n", "{name} dirt"),
            Dirt::Untracked => assert_eq!(st, "?? u.txt\n", "{name} dirt"),
        }
    }
    fx
}

/// Run the hook once; `(decision, reason)`. Non-zero exit panics; only then is
/// an empty stdout read as Allow; non-JSON stdout panics.
fn run_as(fx: &Fx, layout: Layout, command: &str, subagent: bool) -> (String, String) {
    let cwd = fx.cwd(layout);
    let mut p = serde_json::json!({
        "session_id": "otherwt-git-discard-20261120",
        "cwd": cwd,
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    });
    if subagent {
        p["agent_id"] = serde_json::json!("test-subagent");
    }
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    cmd.env_clear()
        .env("HOME", fx.home())
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env("CLAUDE_PROJECT_DIR", fx.repo())
        .env("BLASTGUARD_APPROVALS_DIR", fx.base.join("store"))
        .current_dir(&cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    git_env::scrub(&mut cmd);
    let out = bounded::run_bounded(&mut cmd, p.to_string().as_bytes(), bounded::HOOK_DEADLINE);
    assert!(
        out.status.success(),
        "hook exited {:?} for {command:?}: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return ("allow".into(), String::new());
    }
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("hook stdout for {command:?} is not JSON ({e}): {stdout:?}"));
    let h = &v["hookSpecificOutput"];
    (
        h["permissionDecision"].as_str().unwrap_or("?").to_string(),
        h["permissionDecisionReason"]
            .as_str()
            .unwrap_or("")
            .to_string(),
    )
}

/// The two spellings; `{R}` = target checkout (absolute), `{OP}` = git words.
const FORMS: [(&str, &str); 2] = [("-C", "git -C {R} {OP}"), ("cd&&", "cd {R} && git {OP}")];

const LAYOUTS: [Layout; 2] = [Layout::MainCwd, Layout::OwnWt];

fn render(form: &str, root: &Path, op: &str) -> String {
    form.replace("{R}", root.to_str().unwrap())
        .replace("{OP}", op)
}

/// RED: every (form, layout) against the DIRTY other worktree must be
/// main != allow and subagent == deny.
fn red(op: &str, d: Dirt) {
    let fx = fixture("red", d);
    let root = fx.target(Target::OtherWt);
    let mut bad = Vec::new();
    let mut n = 0usize;
    for (fname, form) in FORMS {
        for layout in LAYOUTS {
            let command = render(form, &root, op);
            let (dm, rm) = run_as(&fx, layout, &command, false);
            let (ds, rs) = run_as(&fx, layout, &command, true);
            n += 1;
            println!(
                "MEASURE red op={op:?} dirt={d:?} form={fname} layout={layout:?} => {dm}/{ds}"
            );
            if dm == "allow" || ds != "deny" {
                bad.push(format!(
                    "op={op:?} dirt={d:?} form={fname} layout={layout:?}: {command:?} \
                     expected main ask|deny + subagent deny, got {dm}/{ds}\n      \
                     main reason: {rm:?}\n      sub reason:  {rs:?}"
                ));
            }
        }
    }
    assert_eq!(n, FORMS.len() * LAYOUTS.len());
    assert!(
        bad.is_empty(),
        "git discard into ANOTHER session's dirty linked worktree is not gated \
         ({} of {n} cells fail-open):\n{}",
        bad.len(),
        bad.join("\n")
    );
}

#[test]
#[ignore = "backlog 606a93ab: fail-open RED; other session's dirty linked worktree, git reset --hard is Allow"]
fn red_other_dirty_reset_hard() {
    red("reset --hard", Dirt::Modified);
}

#[test]
#[ignore = "backlog 606a93ab: fail-open RED; other session's dirty linked worktree, git checkout . is Allow"]
fn red_other_dirty_checkout_dot() {
    red("checkout .", Dirt::Modified);
}

#[test]
#[ignore = "backlog 606a93ab: fail-open RED; other session's dirty linked worktree, git restore <path> is Allow"]
fn red_other_dirty_restore_path() {
    red("restore m.txt", Dirt::Modified);
}

#[test]
#[ignore = "backlog 606a93ab: fail-open RED; other session's dirty linked worktree, git restore . is Allow"]
fn red_other_dirty_restore_dot() {
    red("restore .", Dirt::Modified);
}

#[test]
#[ignore = "backlog 606a93ab: fail-open RED; other session's linked worktree with an untracked file, git clean -fd is Allow"]
fn red_other_untracked_clean_fd() {
    red("clean -fd", Dirt::Untracked);
}

// ---------------------------------------------------------------------------
// Controls (pass today; measured 2026-11-20 at the branch base).
// ---------------------------------------------------------------------------

/// One control row: op, dirt kind, and expected `"main/subagent"` per form
/// in `FORMS` order (`-C`, `cd&&`).
struct Ctl {
    op: &'static str,
    dirt: Dirt,
    exp: [&'static str; 2],
}

fn check_ctl(table: &str, layout: Layout, target: Target, rows: &[Ctl]) {
    let mut bad = Vec::new();
    let mut n = 0usize;
    for row in rows {
        let fx = fixture("ctl", row.dirt);
        let root = fx.target(target);
        for (i, (fname, form)) in FORMS.iter().enumerate() {
            let command = render(form, &root, row.op);
            let (dm, rm) = run_as(&fx, layout, &command, false);
            let (ds, rs) = run_as(&fx, layout, &command, true);
            let got = format!("{dm}/{ds}");
            n += 1;
            println!(
                "MEASURE {table} op={:?} dirt={:?} form={fname} => {got}\n    main: {rm}\n    sub:  {rs}",
                row.op, row.dirt
            );
            if got != row.exp[i] {
                bad.push(format!(
                    "[{table}] op={:?} dirt={:?} form={fname}: {command:?} expected {}, got {got}\n      main reason: {rm:?}\n      sub reason:  {rs:?}",
                    row.op, row.dirt, row.exp[i]
                ));
            }
        }
    }
    assert!(n > 0, "[{table}] nothing run");
    assert!(
        bad.is_empty(),
        "[{table}] {} of {n} control pins moved:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

const A: &str = "allow/allow";

/// Own worktree (session cwd = wt-own, target = wt-own), dirty. The ruling
/// keeps today's verdict here; measured.
#[rustfmt::skip]
const OWN_DIRTY: &[Ctl] = &[
    Ctl { op: "reset --hard", dirt: Dirt::Modified, exp: [A, A] },
    Ctl { op: "checkout .", dirt: Dirt::Modified, exp: [A, A] },
    Ctl { op: "restore m.txt", dirt: Dirt::Modified, exp: [A, A] },
    Ctl { op: "restore .", dirt: Dirt::Modified, exp: [A, A] },
    Ctl { op: "clean -fd", dirt: Dirt::Untracked, exp: [A, A] },
];

#[test]
fn control_own_dirty_worktree_keeps_todays_verdict() {
    check_ctl("OwnDirty", Layout::OwnWt, Target::OwnWt, OWN_DIRTY);
}

/// Clean other worktree; measured today (both layouts).
#[rustfmt::skip]
const OTHER_CLEAN: &[Ctl] = &[
    Ctl { op: "reset --hard", dirt: Dirt::Clean, exp: [A, A] },
    Ctl { op: "checkout .", dirt: Dirt::Clean, exp: [A, A] },
    Ctl { op: "restore m.txt", dirt: Dirt::Clean, exp: [A, A] },
    Ctl { op: "restore .", dirt: Dirt::Clean, exp: [A, A] },
    Ctl { op: "clean -fd", dirt: Dirt::Clean, exp: [A, A] },
];

#[test]
fn control_clean_other_worktree_from_main_cwd() {
    check_ctl(
        "OtherCleanMainCwd",
        Layout::MainCwd,
        Target::OtherWt,
        OTHER_CLEAN,
    );
}

#[test]
fn control_clean_other_worktree_from_own_worktree() {
    check_ctl(
        "OtherCleanOwnWt",
        Layout::OwnWt,
        Target::OtherWt,
        OTHER_CLEAN,
    );
}
