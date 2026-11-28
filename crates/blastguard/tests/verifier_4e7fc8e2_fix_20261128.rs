#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! Independent adversarial verifier for backlog 4e7fc8e2 (blastguard 0.2.1021,
//! branch fix-write-targets 5699ee7ff). Written by a verifier, not the
//! implementer.
//!
//! Every HOLE test first runs the REAL tool (git / install) inside a sandbox
//! under `/private/tmp/claude-wt-verify` with a fake `$HOME`, and asserts the
//! protected path was really written (`PRECONDITION`). Only then is the hook
//! verdict checked (`HOLE`). A failing PRECONDITION means the candidate is not
//! a hole (git refused / wrote elsewhere); a failing HOLE means blastguard
//! Allowed a write git really performs.
//!
//! System-directory cases (`/etc`, `/System`) cannot be written for real and
//! are judged by parity with the already-Deny `cp` landing.
//!
//! Candidates REFUTED by real-git observation 2026-11-28 (git 2.54.0, Apple
//! Git-157), so not asserted:
//! * `git archive -vo F HEAD` — git exits 128 "fatal: Unexpected option
//!   --output"; nothing written.
//! * `cd sub && git checkout-index -a --prefix=../hl/` — exit 0 but nothing
//!   landed at `<toplevel>/../hl`; where it did land was not inspected
//!   (UNVERIFIED that it is exactly cwd-relative).
//! * `git config --file ~/.claude/settings.json k v` on a JSON settings.json —
//!   git exits 128 "bad config line 1"; it rewrites only an absent/empty/INI
//!   file (the hole below uses an absent target behind a link).
//!
//! DOES NOT PROVE: real `$HOME`; Linux tool semantics; any git other than the
//! one on this machine; that an Ask reaches a human.
#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/git_env.rs"]
mod git_env;

use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;

struct Fx {
    _dir: tempfile::TempDir,
    base: PathBuf,
}

impl Fx {
    fn new() -> Fx {
        std::fs::create_dir_all("/private/tmp/claude-wt-verify").unwrap();
        let dir = tempfile::Builder::new()
            .prefix("v4e7-")
            .tempdir_in("/private/tmp/claude-wt-verify")
            .unwrap();
        // Canonical base so symlink-resolved paths compare equal.
        let base = dir.path().canonicalize().unwrap();
        let home = base.join("home");
        std::fs::create_dir_all(home.join(".claude/hooks")).unwrap();
        std::fs::create_dir_all(home.join(".claude/projects")).unwrap();
        std::fs::write(home.join(".claude/settings.json"), "{}").unwrap();
        let work = base.join("work");
        std::fs::create_dir_all(work.join("sub")).unwrap();
        std::fs::create_dir_all(work.join("hooks")).unwrap();
        std::fs::write(work.join("plain.txt"), "x").unwrap();
        // Files whose names become protected once placed under ~/.claude.
        std::fs::write(work.join("settings.json"), "{\"pwned\":1}").unwrap();
        std::fs::write(work.join("hooks/x.sh"), "exit 0").unwrap();
        for args in [
            &["init", "-q"][..],
            &["add", "."][..],
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "-m",
                "init",
            ][..],
        ] {
            let st = git_env::pinned(&work).args(args).status().unwrap();
            assert!(st.success(), "fixture git {args:?}");
        }
        // Untracked symlinks (created after the commit).
        // Dangling: git config refuses to rewrite the JSON settings.json
        // ("bad config line 1"), but creates a new file through a link.
        symlink(home.join(".claude/hooks/cfg"), work.join("get")).unwrap();
        symlink(home.join(".claude/hooks"), base.join("hl")).unwrap();
        Fx { _dir: dir, base }
    }
    fn p(&self, rel: &str) -> PathBuf {
        self.base.join(rel)
    }
    fn s(&self, rel: &str) -> String {
        self.p(rel).display().to_string()
    }
    fn hooks(&self) -> String {
        self.s("home/.claude/hooks")
    }
    fn work(&self) -> PathBuf {
        self.p("work")
    }
}

/// Run a real program in the sandbox (fake HOME, no system config, repo
/// discovery stopped at the sandbox base).
fn real(fx: &Fx, cwd: &Path, prog: &str, args: &[&str], env: &[(&str, String)]) -> String {
    let mut c = Command::new(prog);
    git_env::scrub(&mut c)
        .args(args)
        .current_dir(cwd)
        .env("HOME", fx.p("home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CEILING_DIRECTORIES", &fx.base)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t");
    for (k, v) in env {
        c.env(k, v);
    }
    let out = bounded::run_bounded_stdin(&mut c, bounded::ChildStdin::Null, bounded::TOOL_DEADLINE);
    format!(
        "status={:?} stdout={} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn verdict(fx: &Fx, cwd: &Path, cmd: &str) -> String {
    let payload = serde_json::json!({
        "session_id": "verifier-4e7fc8e2-fix",
        "hook_event_name": "PreToolUse",
        "cwd": cwd,
        "tool_name": "Bash",
        "tool_input": { "command": cmd }
    })
    .to_string();
    let mut hook = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    git_env::scrub(&mut hook)
        .env("HOME", fx.p("home"))
        .env("CLAUDE_PROJECT_DIR", cwd)
        .env("BLASTGUARD_APPROVALS_DIR", fx.p("store"))
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env_remove("BLASTGUARD_ASK")
        .env_remove("TMPDIR")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(&fx.base);
    let out = bounded::run_bounded(&mut hook, payload.as_bytes(), bounded::HOOK_DEADLINE);
    assert_eq!(out.status.code(), Some(0), "hook must always exit 0");
    let s = String::from_utf8_lossy(&out.stdout).into_owned();
    if s.trim().is_empty() {
        return "allow".into();
    }
    let v: serde_json::Value = serde_json::from_str(s.trim()).expect("hook JSON");
    v["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .unwrap_or_else(|| panic!("no permissionDecision: {s}"))
        .to_string()
}

/// A hole: real `prog args` (in `cwd`, extra `env`) writes `landed`, and the
/// hook must not Allow `hook_cmd`.
#[allow(clippy::too_many_arguments)]
fn hole(
    fx: &Fx,
    cwd: &Path,
    prog: &str,
    args: &[&str],
    env: &[(&str, String)],
    landed: &Path,
    hook_cmd: &str,
) {
    let log = real(fx, cwd, prog, args, env);
    assert!(
        landed.exists(),
        "PRECONDITION: real `{prog} {args:?}` did not create {} ({log})",
        landed.display()
    );
    let v = verdict(fx, cwd, hook_cmd);
    assert_ne!(
        v,
        "allow",
        "HOLE: `{hook_cmd}` (cwd {}) => allow, but real {prog} wrote {}",
        cwd.display(),
        landed.display()
    );
}

fn expect_all(fx: &Fx, cwd: &Path, want: &str, cmds: &[String]) {
    let bad: Vec<String> = cmds
        .iter()
        .map(|c| (verdict(fx, cwd, c), c))
        .filter(|(v, _)| {
            if want == "not-allow" {
                v == "allow"
            } else {
                v != want
            }
        })
        .map(|(v, c)| format!("{c} => {v}"))
        .collect();
    assert!(bad.is_empty(), "expected {want}, got:\n{}", bad.join("\n"));
}

// ================================================================ RED: holes

#[test]
fn hole_dash_c_protected_dir_init_without_operand() {
    // `git -C D init` creates D/.git (incl. D/.git/hooks) — no operand to judge.
    let fx = Fx::new();
    let h = fx.hooks();
    hole(
        &fx,
        &fx.work(),
        "git",
        &["-C", &h, "init", "-q"],
        &[],
        &fx.p("home/.claude/hooks/.git/HEAD"),
        &format!("git -C {h} init"),
    );
}

#[test]
fn hole_git_dir_env_prefix_init() {
    // `GIT_DIR=D git init` is the env twin of `git --git-dir=D init` (which Asks).
    let fx = Fx::new();
    let d = format!("{}/gd", fx.hooks());
    hole(
        &fx,
        &fx.work(),
        "git",
        &["init", "-q"],
        &[("GIT_DIR", d.clone())],
        &fx.p("home/.claude/hooks/gd/HEAD"),
        &format!("GIT_DIR={d} git init"),
    );
}

#[test]
fn hole_init_double_dash_operand() {
    // `--` ends options; the implementation truncates `opts` at `--` and so
    // never sees the directory operand after it.
    let fx = Fx::new();
    let d = format!("{}/i2", fx.hooks());
    hole(
        &fx,
        &fx.work(),
        "git",
        &["init", "-q", "--", &d],
        &[],
        &fx.p("home/.claude/hooks/i2/.git/HEAD"),
        &format!("git init -- {d}"),
    );
}

#[test]
fn hole_clone_double_dash_operands() {
    let fx = Fx::new();
    let d = format!("{}/c2", fx.hooks());
    let src = fx.work().display().to_string();
    hole(
        &fx,
        &fx.base,
        "git",
        &["clone", "-q", "--", &src, &d],
        &[],
        &fx.p("home/.claude/hooks/c2/plain.txt"),
        &format!("git clone -- {src} {d}"),
    );
}

#[test]
fn green_clone_abbreviated_value_option_shifts_operands() {
    // `--dep` is git's unique prefix of `--depth`; not in CLONE_VALUE_FLAGS.
    // Observed NOT Allow on 5699ee7ff (caught by another rule; which one is not
    // asserted here) — kept as a regression check.
    let fx = Fx::new();
    let d = format!("{}/c3", fx.hooks());
    let src = format!("file://{}", fx.work().display());
    hole(
        &fx,
        &fx.base,
        "git",
        &["clone", "-q", "--dep", "1", &src, &d],
        &[],
        &fx.p("home/.claude/hooks/c3/plain.txt"),
        &format!("git clone --dep 1 {src} {d}"),
    );
}

#[test]
fn hole_init_abbreviated_value_option_shifts_operand() {
    // `--initial` = `--initial-branch`; its value is taken as the directory.
    let fx = Fx::new();
    let d = format!("{}/i3", fx.hooks());
    hole(
        &fx,
        &fx.work(),
        "git",
        &["init", "-q", "--initial", "main", &d],
        &[],
        &fx.p("home/.claude/hooks/i3/.git/HEAD"),
        &format!("git init --initial main {d}"),
    );
}

#[test]
fn hole_worktree_add_abbreviated_reason_shifts_operand() {
    // `--reas` = `--reason`; its value `r` is taken as the new checkout path.
    let fx = Fx::new();
    let d = format!("{}/w1", fx.hooks());
    hole(
        &fx,
        &fx.work(),
        "git",
        &["worktree", "add", "-q", "--lock", "--reas", "r", &d],
        &[],
        &fx.p("home/.claude/hooks/w1/plain.txt"),
        &format!("git worktree add --lock --reas r {d}"),
    );
}

#[test]
fn hole_format_patch_clustered_short_o() {
    // parse-options clusters short flags: `-ko DIR` = `-k -o DIR`.
    let fx = Fx::new();
    let d = format!("{}/fp", fx.hooks());
    hole(
        &fx,
        &fx.work(),
        "git",
        &["format-patch", "-q", "-ko", &d, "--root"],
        &[],
        &fx.p("home/.claude/hooks/fp/0001-init.patch"),
        &format!("git format-patch -ko {d} --root"),
    );
}

#[test]
fn hole_config_abbreviated_file_named_get_is_not_read_only() {
    // `--fil` is recognised as the file option by the write scan, but
    // `config_is_read_only` does not know `--fil` takes a value, so the file
    // name `get` is read as the `get` subcommand and the write is dropped.
    // `get` is a dangling cwd symlink onto ~/.claude/hooks/cfg.
    let fx = Fx::new();
    hole(
        &fx,
        &fx.work(),
        "git",
        &["config", "--fil", "get", "pwn.key", "v"],
        &[],
        &fx.p("home/.claude/hooks/cfg"),
        "git config --fil get pwn.key v",
    );
}

#[test]
fn control_config_file_named_get_unabbreviated_is_not_allow() {
    // Same write with the full `--file`: positional_operands skips its value,
    // so this one is caught — isolates the abbreviation as the cause above.
    let fx = Fx::new();
    expect_all(
        &fx,
        &fx.work(),
        "not-allow",
        &["git config --file get pwn.key v".to_string()],
    );
}

#[test]
fn green_work_tree_checkout_into_protected() {
    // `--work-tree=~/.claude checkout HEAD -- .` writes the tree's
    // settings.json / hooks/x.sh into ~/.claude.
    let fx = Fx::new();
    let wt = fx.s("home/.claude");
    let log = real(
        &fx,
        &fx.work(),
        "git",
        &[&format!("--work-tree={wt}"), "checkout", "HEAD", "--", "."],
        &[],
    );
    assert!(
        fx.p("home/.claude/hooks/x.sh").exists(),
        "PRECONDITION: real checkout did not land in ~/.claude ({log})"
    );
    let v = verdict(
        &fx,
        &fx.work(),
        &format!("git --work-tree={wt} checkout HEAD -- ."),
    );
    assert_ne!(
        v, "allow",
        "HOLE: --work-tree checkout into ~/.claude => allow"
    );
}

#[test]
fn hole_work_tree_checkout_index_prefix_is_relative_to_work_tree() {
    // checkout-index is NEED_WORK_TREE: git chdirs to the work tree top, so a
    // relative --prefix lands under --work-tree, not the cwd. The
    // implementation drops --work-tree for every target kind.
    let fx = Fx::new();
    let wt = fx.s("home/.claude");
    hole(
        &fx,
        &fx.work(),
        "git",
        &[
            &format!("--work-tree={wt}"),
            "checkout-index",
            "-a",
            "--prefix=hooks/",
        ],
        &[],
        &fx.p("home/.claude/hooks/plain.txt"),
        &format!("git --work-tree={wt} checkout-index -a --prefix=hooks/"),
    );
}

#[test]
fn green_install_d_into_protected() {
    let fx = Fx::new();
    let d = format!("{}/inst", fx.hooks());
    hole(
        &fx,
        &fx.work(),
        "install",
        &["-d", &d],
        &[],
        &fx.p("home/.claude/hooks/inst"),
        &format!("install -d {d}"),
    );
}

#[test]
fn hole_clone_derived_name_into_existing_empty_protected_dir() {
    // Declared residual in docs/specs/blastguard.md ("git clone URL（宛先省略）
    // の導出名は判定しない"). Recorded here as an observed real write: a source
    // repo named `hooks` cloned with cwd ~/.claude fills the empty
    // ~/.claude/hooks.
    let fx = Fx::new();
    let src = fx.p("srcs/hooks");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("y.sh"), "exit 0").unwrap();
    for args in [
        &["init", "-q"][..],
        &["add", "."][..],
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "-m",
            "i",
        ][..],
    ] {
        assert!(git_env::pinned(&src).args(args).status().unwrap().success());
    }
    let s = src.display().to_string();
    hole(
        &fx,
        &fx.p("home/.claude"),
        "git",
        &["clone", "-q", &s],
        &[],
        &fx.p("home/.claude/hooks/y.sh"),
        &format!("git clone {s}"),
    );
}

#[test]
fn green_system_dir_install_variants() {
    // Parity with `cp f /etc/X` (Deny): the same landing via other spellings.
    let fx = Fx::new();
    expect_all(
        &fx,
        &fx.work(),
        "not-allow",
        &[
            "install -d /etc/blastguard_probe".to_string(),
            "install -d /System/blastguard_probe".to_string(),
        ],
    );
}

// ================================================================ GREEN: confirmations

#[test]
fn green_spelling_variants_are_not_allow() {
    let fx = Fx::new();
    let st = fx.s("home/.claude/settings.json");
    let h = fx.hooks();
    expect_all(
        &fx,
        &fx.work(),
        "not-allow",
        &[
            format!("git diff --output {st}"),
            format!("git diff --out={st}"),
            format!("git format-patch -o{h} -1"),
            format!("git checkout-index -a --prefix {h}/"),
            format!("git checkout-index -a --pre={h}/"),
            format!("git config -f{st} a.b c"),
            format!("git config --fil {st} a.b c"),
            format!("git config --file={st} a.b c"),
            format!("git -c x.y=z config --file {st} a.b c"),
            format!("git --git-dir {h}/g init"),
            format!(
                "git -C {} --work-tree=/x diff --output=hooks/a",
                fx.s("home/.claude")
            ),
            format!("git worktree add -b nb {h}"),
            format!("git init --separate-git-dir={h}/s {}", fx.s("newrepo")),
            format!("git init --bare {h}/b"),
            "git config --file ~/.claude/settings.json a.b c".to_string(),
            "git init ~/.claude/hooks/r".to_string(),
            "git config --file \"$HOME/.claude/settings.json\" a.b c".to_string(),
            format!(
                "cd {} && git config -f settings.json a.b c",
                fx.s("home/.claude")
            ),
            format!(
                "cd {} && git format-patch -o hooks -1",
                fx.s("home/.claude")
            ),
            format!("touch -- {h}/t"),
        ],
    );
}

#[test]
fn green_relative_targets_with_cwd_in_dot_claude() {
    let fx = Fx::new();
    expect_all(
        &fx,
        &fx.p("home/.claude"),
        "not-allow",
        &[
            "git config -f settings.json a.b c".to_string(),
            "git init hooks/r".to_string(),
            "touch hooks/t".to_string(),
            "mkdir hooks/d".to_string(),
        ],
    );
}

#[test]
fn green_system_dir_touch_mkdir_variants() {
    let fx = Fx::new();
    expect_all(
        &fx,
        &fx.work(),
        "not-allow",
        &[
            "mkdir -p /etc/blastguard_probe/y".to_string(),
            "touch -- /etc/blastguard_probe".to_string(),
            "mkdir -pm 755 /etc/blastguard_probe".to_string(),
            "touch -c /etc/blastguard_probe".to_string(),
            "touch /private/etc/blastguard_probe".to_string(),
            "mkdir -p /usr/bin/blastguard_probe".to_string(),
        ],
    );
}

// ================================================================ FP controls

#[test]
fn fp_ordinary_writes_stay_allow() {
    let fx = Fx::new();
    expect_all(
        &fx,
        &fx.work(),
        "allow",
        &[
            "git format-patch -o patches/ -1".to_string(),
            "git config -f .gitmodules submodule.x.url u".to_string(),
            "git init sub2".to_string(),
            "git clone https://example.invalid/x.git dir".to_string(),
            "git clone -- https://example.invalid/x.git dir".to_string(),
            "mkdir -p target/x".to_string(),
            "touch src/a.rs".to_string(),
            "git worktree add ../.harness-worktrees/x".to_string(),
            "git worktree add -b x ../.harness-worktrees/x".to_string(),
            "git worktree add ../.harness-worktrees/x -b x".to_string(),
            "git checkout-index -a --prefix=out/".to_string(),
            format!("git init {}", fx.s("scratch/r")),
            format!("mkdir -p {}", fx.s("scratch/a/b")),
            format!("mkdir -p {}", fx.s("home/.claude/projects/p/memory")),
            format!("touch {}", fx.s("home/.claude/projects/p/memory/a.md")),
            "mkdir -p /private/tmp/claude-wt-verify/probe-dir".to_string(),
        ],
    );
}
