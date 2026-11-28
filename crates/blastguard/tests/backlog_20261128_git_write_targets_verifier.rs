#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! Independent verifier for backlog 96faed52 (blastguard 0.2.1008, branch
//! red-git-output 0c1ba7647): git write targets and `touch`/`mkdir` operands
//! routed through the protected-path judgement. Written by a verifier, not the
//! implementer.
//!
//! GREEN part: shapes the implementer claims but the original RED test did
//! not exercise (format-patch -o into a protected subdir, archive -o, bundle
//! create, worktree move, `-C` relative placement, `touch -h`, mkdir,
//! unexpanded values, abbreviations, `--output-indicator-*`, system
//! directories) plus over-block controls.
//!
//! RED part (`#[ignore]`d after RED was observed 2026-11-28 on 0c1ba7647; run
//! with `--ignored`): write shapes that still reach `Allow`. Every one was
//! checked with real git 2026-11-28 in a scratch repo to write the named path:
//!
//! * `git --work-tree=W diff --output=REL` writes `REL` relative to the
//!   process cwd, NOT to `W` (observed: the patch landed in the cwd both with
//!   cwd outside and inside the repo). The fix resolves `REL` against `W`
//!   (`git_moved_dir` returns the work tree), so a cwd symlink onto
//!   `~/.claude` is resolved at the wrong place.
//! * `git format-patch -o DIR` writes `DIR/0001-*.patch`. When `DIR` is the
//!   protected directory itself (`~/.claude/hooks`, `~/.claude`) the verdict
//!   is Allow, while `-o ~/.claude/hooks/sub` Asks.
//! * `git worktree add ~/.claude/hooks` (existing EMPTY dir) populates it.
//! * `git stash show -p --output=F`, `git reflog --output=F`,
//!   `git -c alias.dd=diff dd --output=F` write `F`; `git config --file F`
//!   rewrites `F`; `git checkout-index -a --prefix=DIR/` writes into `DIR`;
//!   `git init DIR` / `git clone SRC DIR` create `DIR`.
//! * `touch /etc/X` / `mkdir /etc/X` are Allow while `cp f /etc/X` and
//!   `git diff --output=/etc/X` are Deny.
//!
//! Over-block controls were compared 2026-11-28 against the deployed
//! pre-fix binary (plugin cache 0.2.1005, deployed from f60cd1d6e): every
//! control below was Allow there and is Allow here.
//!
//! Observed, not asserted: macOS `touch -h LINKDIR/new` (parent symlink,
//! file absent) exits 0 and creates nothing, so the lexical `touch -h`
//! verdict is not a hole. `git diff --ou=F` is rejected by git itself
//! ("invalid option"); the Ask on it is harmless strictness.
//!
//! DOES NOT PROVE: real `$HOME`; that the hook's Ask reaches a human; Linux
//! touch semantics; any git version other than the one on this machine.
#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/git_env.rs"]
mod git_env;

use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::process::{Command, Stdio};

struct Fx {
    _dir: tempfile::TempDir,
    base: PathBuf,
}

impl Fx {
    fn new() -> Fx {
        std::fs::create_dir_all("/private/tmp/claude-askrules").unwrap();
        let dir = tempfile::Builder::new()
            .prefix("git-write-targets-verifier-")
            .tempdir_in("/private/tmp/claude-askrules")
            .unwrap();
        let base = dir.path().to_path_buf();
        let home = base.join("home");
        std::fs::create_dir_all(home.join(".claude/hooks")).unwrap();
        std::fs::write(home.join(".claude/settings.json"), "{}").unwrap();
        let work = base.join("work");
        std::fs::create_dir_all(work.join("sub")).unwrap();
        std::fs::write(work.join("plain.txt"), "x").unwrap();
        for args in [
            &["init", "-q"][..],
            &["add", "plain.txt"][..],
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
        // Dangling file link into hooks/, and directory links onto hooks/ and .claude/.
        symlink(home.join(".claude/hooks/new.sh"), work.join("dlink")).unwrap();
        symlink(home.join(".claude/hooks"), work.join("hlink")).unwrap();
        symlink(home.join(".claude"), work.join("clink")).unwrap();
        assert!(!work.join("dlink").exists(), "fixture: dlink must dangle");
        assert!(work.join("clink/settings.json").is_file(), "fixture: clink");
        Fx { _dir: dir, base }
    }
    fn home(&self) -> String {
        self.base.join("home").display().to_string()
    }
    fn work(&self) -> String {
        self.base.join("work").display().to_string()
    }
    fn settings(&self) -> String {
        format!("{}/.claude/settings.json", self.home())
    }
    fn hooks(&self) -> String {
        format!("{}/.claude/hooks", self.home())
    }
}

fn verdict(fx: &Fx, cmd: &str) -> String {
    let cwd = fx.base.join("work");
    let payload = serde_json::json!({
        "session_id": "git-write-targets-verifier",
        "hook_event_name": "PreToolUse",
        "cwd": cwd,
        "tool_name": "Bash",
        "tool_input": { "command": cmd }
    })
    .to_string();
    let mut hook = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    git_env::scrub(&mut hook)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("HOME", fx.base.join("home"))
        .env("CLAUDE_PROJECT_DIR", &cwd)
        .env("BLASTGUARD_APPROVALS_DIR", fx.base.join("store"))
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

/// Commands whose verdict is not `want` (`"not-allow"` = anything but allow).
fn mismatches(fx: &Fx, want: &str, cmds: Vec<String>) -> Vec<String> {
    cmds.into_iter()
        .map(|c| (verdict(fx, &c), c))
        .filter(|(v, _)| {
            if want == "not-allow" {
                v == "allow"
            } else {
                v != want
            }
        })
        .map(|(v, c)| format!("{c} => {v}"))
        .collect()
}

fn expect(fx: &Fx, want: &str, cmds: Vec<String>) {
    let bad = mismatches(fx, want, cmds);
    assert!(bad.is_empty(), "expected {want}, got:\n{}", bad.join("\n"));
}

// ---------------------------------------------------------------- GREEN

#[test]
fn format_patch_output_dir_into_protected_subdir_is_not_allow() {
    let fx = Fx::new();
    let d = format!("{}/sub", fx.hooks());
    expect(
        &fx,
        "not-allow",
        vec![
            format!("git format-patch -o {d} -1"),
            format!("git format-patch -o{d} -1"),
            format!("git format-patch --output-directory {d} -1"),
            format!("git format-patch --output-directory={d} -1"),
            format!("git format-patch -1 --output={}", fx.settings()),
        ],
    );
}

#[test]
fn archive_output_to_protected_is_not_allow() {
    let fx = Fx::new();
    let s = fx.settings();
    expect(
        &fx,
        "not-allow",
        vec![
            format!("git archive -o {s} HEAD"),
            format!("git archive -o{s} HEAD"),
            format!("git archive --output={s} HEAD"),
            "git archive -o hlink/a.tar HEAD".to_string(),
        ],
    );
}

#[test]
fn bundle_create_to_protected_is_not_allow() {
    let fx = Fx::new();
    let s = fx.settings();
    expect(
        &fx,
        "not-allow",
        vec![
            format!("git bundle create {s} HEAD"),
            format!("git bundle create -q {s} HEAD"),
            "git bundle create hlink/b HEAD".to_string(),
        ],
    );
}

#[test]
fn worktree_add_and_move_under_protected_is_not_allow() {
    let fx = Fx::new();
    let h = fx.hooks();
    expect(
        &fx,
        "not-allow",
        vec![
            format!("git worktree move wtx {h}/w"),
            format!("git worktree add -b nb {h}/w2"),
            format!("git worktree add --lock --reason r {h}/w3"),
        ],
    );
}

#[test]
fn dash_c_relative_output_is_placed_in_the_moved_dir() {
    let fx = Fx::new();
    let h = fx.home();
    expect(
        &fx,
        "not-allow",
        vec![
            format!("git -C {h} diff --output=.claude/settings.json"),
            format!("git -C {h}/.claude diff --output=settings.json"),
            "git -C ~/.claude diff --output=settings.json".to_string(),
            // `clink` is a cwd symlink onto ~/.claude: only physical resolution catches it.
            "git -C clink diff --output=settings.json".to_string(),
            format!("git -C {} diff --output=hlink/x.sh", fx.work()),
            "git diff --output=../home/.claude/settings.json".to_string(),
        ],
    );
}

#[test]
fn work_tree_with_lexically_protected_name_is_not_allow() {
    // Caught by the lexical `.claude/settings.json` name, so this does NOT
    // prove where `--work-tree` places a relative target (see the RED below).
    let fx = Fx::new();
    expect(
        &fx,
        "not-allow",
        vec![format!(
            "git --work-tree={} diff --output=.claude/settings.json",
            fx.home()
        )],
    );
}

#[test]
fn touch_and_mkdir_through_links_into_protected_are_not_allow() {
    let fx = Fx::new();
    let h = fx.hooks();
    expect(
        &fx,
        "not-allow",
        vec![
            // A value-taking option must not hide the operand after it.
            "touch -d 2020-01-01 dlink".to_string(),
            "touch hlink/newfile".to_string(),
            "env touch dlink".to_string(),
            "command touch dlink".to_string(),
            "sudo touch dlink".to_string(),
            "mkdir hlink/newdir".to_string(),
            "mkdir -p hlink/a/b".to_string(),
            "command mkdir -p hlink/z".to_string(),
            format!("mkdir {h}/x"),
            format!("mkdir -p {h}/x"),
            format!("mkdir -m 755 {h}/x"),
        ],
    );
}

#[test]
fn touch_no_dereference_on_link_itself_is_allow() {
    // `touch -h` changes the link, never its target (macOS touch -h on a
    // dangling link observed to create nothing).
    let fx = Fx::new();
    expect(
        &fx,
        "allow",
        vec!["touch -h dlink".to_string(), "touch -h hlink".to_string()],
    );
}

#[test]
fn unexpanded_or_missing_output_value_is_not_allow() {
    let fx = Fx::new();
    expect(
        &fx,
        "not-allow",
        vec![
            "git diff --output=$X".to_string(),
            "git diff --output=\"$HOME/.claude/settings.json\"".to_string(),
            "git diff --output=`pwd`/x".to_string(),
            "git diff --output".to_string(),
            "git diff --output=".to_string(),
        ],
    );
}

#[test]
fn quoted_and_abbreviated_output_to_protected_is_not_allow() {
    let fx = Fx::new();
    let s = fx.settings();
    expect(
        &fx,
        "not-allow",
        vec![
            format!("git diff --ou={s}"),
            format!("git diff \"--output={s}\""),
            format!("git diff '--output={s}'"),
            format!("git diff --output \"{s}\""),
            format!("git \"diff\" --output={s}"),
        ],
    );
}

#[test]
fn output_indicator_options_are_not_write_targets() {
    let fx = Fx::new();
    expect(
        &fx,
        "allow",
        vec![
            "git diff --output-indicator-new=x".to_string(),
            format!("git diff --output-indicator-new={}", fx.settings()),
            "git log --output-indicator-old=+ -1".to_string(),
        ],
    );
}

#[test]
fn git_output_into_system_dirs_is_deny() {
    let fx = Fx::new();
    expect(
        &fx,
        "deny",
        vec![
            "git diff --output=/etc/blastguard_probe".to_string(),
            "git diff --output=/usr/bin/blastguard_probe".to_string(),
            "git log --output=/System/blastguard_probe".to_string(),
            // control for the RED below: the same landing via cp is Deny.
            "cp plain.txt /etc/blastguard_probe".to_string(),
        ],
    );
}

#[test]
fn control_ordinary_writes_inside_worktree_stay_allow() {
    // Allow on the pre-fix deployed binary (0.2.1005 / f60cd1d6e) too.
    let fx = Fx::new();
    expect(
        &fx,
        "allow",
        vec![
            "git diff --output=a.patch".to_string(),
            format!("git diff --output={}/a.patch", fx.work()),
            "git format-patch -o out/ -1".to_string(),
            "git archive -o out.tar HEAD".to_string(),
            "git bundle create repo.bundle HEAD".to_string(),
            "git worktree add ../wt-new".to_string(),
            "git worktree add /private/tmp/claude-askrules/wtx".to_string(),
            "touch newfile".to_string(),
            "mkdir -p build/x".to_string(),
        ],
    );
}

// ---------------------------------------------------------------- RED

#[test]
#[ignore = "RED observed 2026-11-28 on 0c1ba7647 (verifier of 96faed52): Allow"]
fn red_work_tree_relative_output_is_resolved_from_cwd() {
    // Real git: `--output=REL` is relative to the process cwd, not --work-tree.
    let fx = Fx::new();
    let wt = "/private/tmp/claude-askrules/nowhere";
    expect(
        &fx,
        "not-allow",
        vec![
            format!("git --work-tree={wt} diff --output=clink/settings.json"),
            format!("git --work-tree={wt} diff --output=hlink/pre.sh"),
            format!("git -C sub --work-tree={wt} diff --output=../clink/settings.json"),
        ],
    );
}

#[test]
#[ignore = "RED observed 2026-11-28 on 0c1ba7647 (verifier of 96faed52): Allow"]
fn red_format_patch_output_dir_is_the_protected_dir_itself() {
    let fx = Fx::new();
    let h = fx.hooks();
    expect(
        &fx,
        "not-allow",
        vec![
            format!("git format-patch -o {h} -1"),
            format!("git format-patch -o{h} -1"),
            format!("git format-patch --output-directory {h} -1"),
            format!("git format-patch --output-directory={h} -1"),
            "git format-patch -o hlink -1".to_string(),
            format!("git format-patch -o {}/.claude -1", fx.home()),
        ],
    );
}

#[test]
#[ignore = "RED observed 2026-11-28 on 0c1ba7647 (verifier of 96faed52): Allow"]
fn red_worktree_add_into_empty_protected_dir_itself() {
    // Fixture hooks/ is empty; real git populates an existing empty dir.
    let fx = Fx::new();
    expect(
        &fx,
        "not-allow",
        vec![format!("git worktree add {}", fx.hooks())],
    );
}

#[test]
#[ignore = "RED observed 2026-11-28 on 0c1ba7647 (verifier of 96faed52): Allow"]
fn red_diff_output_via_other_subcommands() {
    let fx = Fx::new();
    let s = fx.settings();
    expect(
        &fx,
        "not-allow",
        vec![
            format!("git stash show -p --output={s}"),
            format!("git reflog --output={s}"),
            format!("git -c alias.dd=diff dd --output={s}"),
        ],
    );
}

#[test]
#[ignore = "RED observed 2026-11-28 on 0c1ba7647 (verifier of 96faed52): Allow"]
fn red_git_config_file_rewrites_protected() {
    let fx = Fx::new();
    let s = fx.settings();
    expect(
        &fx,
        "not-allow",
        vec![
            format!("git config --file {s} a.b c"),
            format!("git config -f {s} a.b c"),
        ],
    );
}

#[test]
#[ignore = "RED observed 2026-11-28 on 0c1ba7647 (verifier of 96faed52): Allow"]
fn red_checkout_index_prefix_into_protected() {
    let fx = Fx::new();
    expect(
        &fx,
        "not-allow",
        vec![format!("git checkout-index -a --prefix={}/", fx.hooks())],
    );
}

#[test]
#[ignore = "RED observed 2026-11-28 on 0c1ba7647 (verifier of 96faed52): Allow"]
fn red_init_and_clone_create_under_protected() {
    let fx = Fx::new();
    let h = fx.hooks();
    expect(
        &fx,
        "not-allow",
        vec![
            format!("git init {h}/r"),
            format!("git clone {} {h}/c", fx.work()),
            format!("git --git-dir={h}/g init"),
        ],
    );
}

#[test]
#[ignore = "RED observed 2026-11-28 on 0c1ba7647 (verifier of 96faed52): Allow"]
fn red_touch_mkdir_into_system_dirs_like_cp() {
    // cp / tee / git --output to /etc are Deny; touch / mkdir there are Allow.
    let fx = Fx::new();
    expect(
        &fx,
        "not-allow",
        vec![
            "touch /etc/blastguard_probe".to_string(),
            "mkdir /etc/blastguard_probe".to_string(),
            "mkdir -p /System/blastguard_probe".to_string(),
        ],
    );
}
