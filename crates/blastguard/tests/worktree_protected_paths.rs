// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Protected-NAMED paths inside a linked worktree checkout are the worktree's
//! own files, not the gates that guard this session (user ruling 2026-10-03,
//! verbatim: 「worktreeの編集や削除を拒むのはむしろ不正なのでなおしてほしい」).
//!
//! What this file pins, against the REAL hook binary and a REAL on-disk layout
//! (scope.rs probes the filesystem, so the worktree storage root and the
//! checkouts exist; the checkouts are real `git worktree add` checkouts):
//!
//!   * a Write / Edit / MultiEdit, or a Bash delete/edit verb, aimed at a
//!     protected-named path STRICTLY INSIDE a linked worktree checkout under a
//!     worktree storage root (`<parent>/.harness-worktrees/<wt>/…`,
//!     `$HOME/.condukt/worktrees/<wt>/…`) is Allow;
//!   * the same relative path in the MAIN tree stays Deny — and every positive
//!     case is PAIRED with that main-tree twin inside the same test, so a run in
//!     which nothing is ever denied (a fixture that silently stopped exercising
//!     the protected rule) fails instead of passing;
//!   * unchanged Deny: `$HOME/.claude/settings.json`, `$HOME/.zshrc`, a `..`
//!     spelling that climbs out of a worktree into the main tree, the storage
//!     root itself, symlinks inside a worktree that lead into the main tree's
//!     protected paths, and `.harness-worktrees` directories scope.rs does not
//!     accept as roots;
//!   * operands that cannot be placed (`~`, glob) are never asserted Allow.
//!
//! Ask is disabled (`BLASTGUARD_ASK=never`) so the hook's `Ask` resolves to
//! `Deny` deterministically regardless of the environment the tests run in:
//! "refused" is then exactly `"permissionDecision":"deny"`, and Allow is exactly
//! empty stdout.
//!
//! The base is [`neutral_base::neutral_base`] (see that module): a calibrated
//! location whose own path neither sits inside a real `.harness-worktrees` nor a
//! blastguard temp safe root, so the fixture layout — not where the test binary
//! happens to run — decides the verdict.

#[path = "support/neutral_base.rs"]
mod neutral_base;

use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Fixture {
    base: PathBuf,
    home: PathBuf,
    /// The main tree (the project dir itself). Named `harness` so that
    /// `<wt>/../../harness/…` spells a path into it.
    proj: PathBuf,
    /// `<base>/src/.harness-worktrees` — the storage root derived from `proj`.
    hw: PathBuf,
    /// `<hw>/wt1` — a real linked worktree of `proj`.
    wt: PathBuf,
    /// `$HOME/.condukt/worktrees/run-x` — a real linked worktree of `proj`.
    cwt: PathBuf,
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git spawns");
    assert!(
        out.status.success(),
        "git {args:?} failed in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn write(p: &Path, content: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let base = neutral_base::neutral_base()
            .join("worktree_protected_paths")
            .join(name);
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        let proj = base.join("src/harness");
        let hw = base.join("src/.harness-worktrees");
        let wt = hw.join("wt1");
        let cwt = home.join(".condukt/worktrees/run-x");

        // $HOME files that must stay protected.
        write(&home.join(".claude/settings.json"), "{}\n");
        write(&home.join(".zshrc"), "# rc\n");

        // Main tree: a real git repo carrying protected-named files.
        std::fs::create_dir_all(&proj).unwrap();
        git(&proj, &["init", "-q"]);
        write(&proj.join(".githooks/pre-push"), "#!/bin/sh\nexit 0\n");
        write(&proj.join(".githooks/pre-commit"), "#!/bin/sh\nexit 0\n");
        write(&proj.join(".claude/settings.json"), "{\"a\":1}\n");
        write(&proj.join(".claude/hooks/h.sh"), "#!/bin/sh\n");
        write(&proj.join("scripts/x.py"), "print(1)\n");
        git(&proj, &["add", "-A"]);
        git(&proj, &["commit", "-q", "-m", "init"]);

        // Linked worktrees under both storage-root kinds.
        std::fs::create_dir_all(&hw).unwrap();
        std::fs::create_dir_all(cwt.parent().unwrap()).unwrap();
        git(
            &proj,
            &["worktree", "add", "-q", "-b", "wt1", wt.to_str().unwrap()],
        );
        git(
            &proj,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "run-x",
                cwt.to_str().unwrap(),
            ],
        );

        // Untracked empty dirs (rmdir targets) in every tree.
        for t in [&proj, &wt, &cwt] {
            std::fs::create_dir_all(t.join("emptydir")).unwrap();
            std::fs::create_dir_all(t.join(".claude/hooks/sub")).unwrap();
        }

        Fixture {
            base,
            home,
            proj,
            hw,
            wt,
            cwt,
        }
    }

    /// Hook stdout for one tool call, with the given payload cwd and
    /// CLAUDE_PROJECT_DIR.
    fn run_tool(&self, tool: &str, input: Value, cwd: &Path, project_dir: &Path) -> String {
        let payload = json!({
            "session_id": "worktree-protected-paths",
            "hook_event_name": "PreToolUse",
            "cwd": cwd.display().to_string(),
            "tool_name": tool,
            "tool_input": input,
        })
        .to_string();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("CLAUDE_PROJECT_DIR", project_dir)
            .env("BLASTGUARD_APPROVALS_DIR", self.base.join("store"))
            .env("HOME", &self.home)
            // Deterministic: Ask resolves to Deny, so "refused" == deny.
            .env("BLASTGUARD_ASK", "never")
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
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Session in the MAIN tree (cwd = CLAUDE_PROJECT_DIR = main tree).
    fn tool(&self, tool: &str, input: Value) -> String {
        self.run_tool(tool, input, &self.proj, &self.proj)
    }

    fn bash(&self, command: &str) -> String {
        self.tool("Bash", json!({ "command": command }))
    }

    /// Session INSIDE the linked worktree (cwd = CLAUDE_PROJECT_DIR = wt1).
    fn tool_from_wt(&self, tool: &str, input: Value) -> String {
        self.run_tool(tool, input, &self.wt, &self.wt)
    }
}

fn d(p: &Path) -> String {
    p.display().to_string()
}

fn is_allow(out: &str) -> bool {
    out.trim().is_empty()
}
fn is_deny(out: &str) -> bool {
    out.contains(r#""permissionDecision":"deny""#)
}

#[track_caller]
fn assert_allow(case: &str, out: &str) {
    assert!(
        is_allow(out),
        "{case}: inside a linked worktree this must be Allow (no output); got {out}"
    );
}
/// Deny, and specifically by the protected-path family of rules.
#[track_caller]
fn assert_protected_deny(case: &str, out: &str) {
    assert!(is_deny(out), "{case}: must be Deny; got {out}");
    assert!(
        out.contains("protected"),
        "{case}: must be denied by a protected-path rule; got {out}"
    );
}
#[track_caller]
fn assert_deny(case: &str, out: &str) {
    assert!(is_deny(out), "{case}: must be Deny; got {out}");
}
#[track_caller]
fn assert_not_allow(case: &str, out: &str) {
    assert!(
        !is_allow(out),
        "{case}: must NOT be Allow; got empty output"
    );
}

/// Main-tree twin denied FIRST (anti-vacuity), then the worktree case allowed.
#[track_caller]
fn pair(case: &str, main_out: &str, wt_out: &str) {
    assert_protected_deny(&format!("[main-tree twin] {case}"), main_out);
    assert_allow(&format!("[worktree] {case}"), wt_out);
}

// ===========================================================================
// 1. Write / Edit / MultiEdit inside a linked worktree: Allow
// ===========================================================================

#[test]
fn write_githooks_pre_push_in_harness_worktree() {
    let f = Fixture::new("w_githooks");
    let input = |p: &Path| json!({ "file_path": d(p), "content": "#!/bin/sh\nexit 1\n" });
    pair(
        "Write .githooks/pre-push",
        &f.tool("Write", input(&f.proj.join(".githooks/pre-push"))),
        &f.tool("Write", input(&f.wt.join(".githooks/pre-push"))),
    );
}

#[test]
fn write_claude_settings_in_harness_worktree() {
    let f = Fixture::new("w_settings");
    let input = |p: &Path| json!({ "file_path": d(p), "content": "{\"b\":2}\n" });
    pair(
        "Write .claude/settings.json",
        &f.tool("Write", input(&f.proj.join(".claude/settings.json"))),
        &f.tool("Write", input(&f.wt.join(".claude/settings.json"))),
    );
}

#[test]
fn write_claude_hooks_file_in_harness_worktree() {
    let f = Fixture::new("w_hooks");
    let input = |p: &Path| json!({ "file_path": d(p), "content": "#!/bin/sh\necho hi\n" });
    pair(
        "Write .claude/hooks/h.sh",
        &f.tool("Write", input(&f.proj.join(".claude/hooks/h.sh"))),
        &f.tool("Write", input(&f.wt.join(".claude/hooks/h.sh"))),
    );
}

#[test]
fn edit_githooks_pre_push_in_harness_worktree() {
    let f = Fixture::new("e_githooks");
    let input =
        |p: &Path| json!({ "file_path": d(p), "old_string": "exit 0", "new_string": "exit 1" });
    pair(
        "Edit .githooks/pre-push",
        &f.tool("Edit", input(&f.proj.join(".githooks/pre-push"))),
        &f.tool("Edit", input(&f.wt.join(".githooks/pre-push"))),
    );
}

#[test]
fn multiedit_claude_settings_in_harness_worktree() {
    let f = Fixture::new("me_settings");
    let input = |p: &Path| {
        json!({
            "file_path": d(p),
            "edits": [ { "old_string": "\"a\":1", "new_string": "\"a\":2" } ],
        })
    };
    pair(
        "MultiEdit .claude/settings.json",
        &f.tool("MultiEdit", input(&f.proj.join(".claude/settings.json"))),
        &f.tool("MultiEdit", input(&f.wt.join(".claude/settings.json"))),
    );
}

#[test]
fn write_githooks_in_condukt_worktree() {
    let f = Fixture::new("w_condukt");
    let input = |p: &Path| json!({ "file_path": d(p), "content": "#!/bin/sh\nexit 1\n" });
    pair(
        "Write ~/.condukt/worktrees/run-x/.githooks/pre-commit",
        &f.tool("Write", input(&f.proj.join(".githooks/pre-commit"))),
        &f.tool("Write", input(&f.cwt.join(".githooks/pre-commit"))),
    );
}

#[test]
fn edit_githooks_in_condukt_worktree() {
    let f = Fixture::new("e_condukt");
    let input =
        |p: &Path| json!({ "file_path": d(p), "old_string": "exit 0", "new_string": "exit 1" });
    pair(
        "Edit ~/.condukt/worktrees/run-x/.githooks/pre-commit",
        &f.tool("Edit", input(&f.proj.join(".githooks/pre-commit"))),
        &f.tool("Edit", input(&f.cwt.join(".githooks/pre-commit"))),
    );
}

/// The session runs INSIDE the worktree (cwd = CLAUDE_PROJECT_DIR = wt1, the
/// §8 layout). Its own checkout's protected-named files are editable; the main
/// tree's are not.
#[test]
fn write_from_a_session_inside_the_worktree() {
    let f = Fixture::new("w_from_wt");
    let input = |p: &Path| json!({ "file_path": d(p), "content": "#!/bin/sh\nexit 1\n" });
    pair(
        "Write .githooks/pre-push (session cwd = wt1)",
        &f.tool_from_wt("Write", input(&f.proj.join(".githooks/pre-push"))),
        &f.tool_from_wt("Write", input(&f.wt.join(".githooks/pre-push"))),
    );
}

// ===========================================================================
// 2. Bash delete/edit verbs inside a linked worktree: Allow
// ===========================================================================

#[test]
fn bash_rm_githooks_file_in_harness_worktree() {
    let f = Fixture::new("b_rm_githooks");
    let rel = ".githooks/pre-push";
    pair(
        "rm <tree>/.githooks/pre-push",
        &f.bash(&format!("rm {}", d(&f.proj.join(rel)))),
        &f.bash(&format!("rm {}", d(&f.wt.join(rel)))),
    );
}

#[test]
fn bash_rm_claude_settings_in_harness_worktree() {
    let f = Fixture::new("b_rm_settings");
    let rel = ".claude/settings.json";
    pair(
        "rm -f <tree>/.claude/settings.json",
        &f.bash(&format!("rm -f {}", d(&f.proj.join(rel)))),
        &f.bash(&format!("rm -f {}", d(&f.wt.join(rel)))),
    );
}

#[test]
fn bash_rm_githooks_file_in_condukt_worktree() {
    let f = Fixture::new("b_rm_condukt");
    let rel = ".githooks/pre-commit";
    pair(
        "rm <tree>/.githooks/pre-commit (condukt worktree)",
        &f.bash(&format!("rm {}", d(&f.proj.join(rel)))),
        &f.bash(&format!("rm {}", d(&f.cwt.join(rel)))),
    );
}

#[test]
fn bash_rmdir_protected_dir_in_harness_worktree() {
    let f = Fixture::new("b_rmdir_hooks");
    let rel = ".claude/hooks/sub";
    pair(
        "rmdir <tree>/.claude/hooks/sub",
        &f.bash(&format!("rmdir {}", d(&f.proj.join(rel)))),
        &f.bash(&format!("rmdir {}", d(&f.wt.join(rel)))),
    );
}

#[test]
fn bash_sed_i_githooks_in_harness_worktree() {
    let f = Fixture::new("b_sed");
    let rel = ".githooks/pre-push";
    pair(
        "sed -i <tree>/.githooks/pre-push",
        &f.bash(&format!(
            "sed -i 's/exit 0/exit 1/' {}",
            d(&f.proj.join(rel))
        )),
        &f.bash(&format!("sed -i 's/exit 0/exit 1/' {}", d(&f.wt.join(rel)))),
    );
}

#[test]
fn bash_redirect_githooks_in_harness_worktree() {
    let f = Fixture::new("b_redirect");
    let rel = ".githooks/pre-push";
    pair(
        "echo x > <tree>/.githooks/pre-push",
        &f.bash(&format!("echo x > {}", d(&f.proj.join(rel)))),
        &f.bash(&format!("echo x > {}", d(&f.wt.join(rel)))),
    );
}

/// `chmod -x` (clearing the exec bit disarms a hook) is what the main tree
/// denies, so it is the paired form. `chmod +x` is not denied even in the main
/// tree (measured: empty output), so it is asserted unpaired below.
#[test]
fn bash_chmod_githooks_in_harness_worktree() {
    let f = Fixture::new("b_chmod");
    let rel = ".githooks/pre-push";
    pair(
        "chmod -x <tree>/.githooks/pre-push",
        &f.bash(&format!("chmod -x {}", d(&f.proj.join(rel)))),
        &f.bash(&format!("chmod -x {}", d(&f.wt.join(rel)))),
    );
    assert_allow(
        "chmod +x <wt>/.githooks/pre-push",
        &f.bash(&format!("chmod +x {}", d(&f.wt.join(rel)))),
    );
}

/// Non-protected deletions inside a worktree. These are not paired with a
/// main-tree Deny: they are not protected paths, so the main tree is not
/// required to deny them (the anti-vacuity for this file comes from the paired
/// tests above). They pin the ruling's general half — refusing a deletion
/// inside a worktree is itself wrong.
#[test]
fn bash_rm_f_plain_file_in_harness_worktree() {
    let f = Fixture::new("b_rm_plain");
    let out = f.bash(&format!("rm -f {}", d(&f.wt.join("scripts/x.py"))));
    assert_allow("rm -f <wt>/scripts/x.py", &out);
}

#[test]
fn bash_rmdir_plain_dir_in_harness_worktree() {
    let f = Fixture::new("b_rmdir_plain");
    let out = f.bash(&format!("rmdir {}", d(&f.wt.join("emptydir"))));
    assert_allow("rmdir <wt>/emptydir", &out);
}

// ===========================================================================
// 3. Must stay denied
// ===========================================================================

#[test]
fn home_claude_settings_stays_denied() {
    let f = Fixture::new("n_home_settings");
    let p = f.home.join(".claude/settings.json");
    assert_protected_deny(
        "Write $HOME/.claude/settings.json",
        &f.tool("Write", json!({ "file_path": d(&p), "content": "{}\n" })),
    );
    assert_protected_deny(
        "rm $HOME/.claude/settings.json",
        &f.bash(&format!("rm {}", d(&p))),
    );
    // Also from a session inside the worktree.
    assert_protected_deny(
        "Write $HOME/.claude/settings.json (session cwd = wt1)",
        &f.tool_from_wt("Write", json!({ "file_path": d(&p), "content": "{}\n" })),
    );
}

#[test]
fn home_zshrc_stays_denied() {
    let f = Fixture::new("n_zshrc");
    let p = f.home.join(".zshrc");
    assert_protected_deny(
        "Write $HOME/.zshrc",
        &f.tool("Write", json!({ "file_path": d(&p), "content": "evil\n" })),
    );
    assert_protected_deny("rm $HOME/.zshrc", &f.bash(&format!("rm {}", d(&p))));
    assert_protected_deny(
        "echo x >> $HOME/.zshrc",
        &f.bash(&format!("echo x >> {}", d(&p))),
    );
}

/// `<wt>/../../harness/.githooks/pre-push` starts with the worktree's spelling
/// but resolves into the MAIN tree.
#[test]
fn dotdot_climbing_out_of_worktree_stays_denied() {
    let f = Fixture::new("n_dotdot");
    let p = format!("{}/../../harness/.githooks/pre-push", d(&f.wt));
    assert_protected_deny(
        "Write <wt>/../../harness/.githooks/pre-push",
        &f.tool(
            "Write",
            json!({ "file_path": p, "content": "#!/bin/sh\nexit 1\n" }),
        ),
    );
    assert_protected_deny(
        "Edit <wt>/../../harness/.githooks/pre-push",
        &f.tool(
            "Edit",
            json!({ "file_path": p, "old_string": "exit 0", "new_string": "exit 1" }),
        ),
    );
    assert_protected_deny(
        "rm <wt>/../../harness/.githooks/pre-push",
        &f.bash(&format!("rm {p}")),
    );
    assert_protected_deny(
        "echo x > <wt>/../../harness/.githooks/pre-push",
        &f.bash(&format!("echo x > {p}")),
    );
    // A `..` that climbs only to the storage root and back into the main tree.
    let p2 = format!("{}/../../harness/.claude/settings.json", d(&f.wt));
    assert_protected_deny(
        "Write <wt>/../../harness/.claude/settings.json",
        &f.tool("Write", json!({ "file_path": p2, "content": "{}\n" })),
    );
}

#[test]
fn storage_roots_themselves_stay_denied() {
    let f = Fixture::new("n_roots");
    assert_deny(
        "rm -r <base>/src/.harness-worktrees",
        &f.bash(&format!("rm -r {}", d(&f.hw))),
    );
    assert_deny(
        "rm -rf $HOME/.condukt/worktrees",
        &f.bash(&format!("rm -rf {}", d(&f.home.join(".condukt/worktrees")))),
    );
}

/// `<wt2>/.githooks` is a symlink to the MAIN tree's `.githooks`: the path is
/// spelled inside a worktree, but the bytes it reaches are the main tree's
/// gates.
#[test]
fn symlink_inside_worktree_into_main_tree_stays_denied() {
    let f = Fixture::new("n_symlink");
    let wt2 = f.hw.join("wt2");
    git(
        &f.proj,
        &["worktree", "add", "-q", "-b", "wt2", wt2.to_str().unwrap()],
    );
    std::fs::remove_dir_all(wt2.join(".githooks")).unwrap();
    std::os::unix::fs::symlink(f.proj.join(".githooks"), wt2.join(".githooks")).unwrap();
    let p = wt2.join(".githooks/pre-push");
    assert_protected_deny(
        "Write <wt2>/.githooks/pre-push (.githooks -> main tree)",
        &f.tool(
            "Write",
            json!({ "file_path": d(&p), "content": "#!/bin/sh\nexit 1\n" }),
        ),
    );
    assert_protected_deny(
        "Edit <wt2>/.githooks/pre-push (.githooks -> main tree)",
        &f.tool(
            "Edit",
            json!({ "file_path": d(&p), "old_string": "exit 0", "new_string": "exit 1" }),
        ),
    );
    assert_protected_deny(
        "rm <wt2>/.githooks/pre-push (.githooks -> main tree)",
        &f.bash(&format!("rm {}", d(&p))),
    );
    assert_protected_deny(
        "echo x > <wt2>/.githooks/pre-push (.githooks -> main tree)",
        &f.bash(&format!("echo x > {}", d(&p))),
    );

    // A whole "worktree" entry that is a symlink to the main tree.
    let link = f.hw.join("wtlink");
    std::os::unix::fs::symlink(&f.proj, &link).unwrap();
    let p2 = link.join(".githooks/pre-push");
    assert_protected_deny(
        "Write <hw>/wtlink/.githooks/pre-push (wtlink -> main tree)",
        &f.tool(
            "Write",
            json!({ "file_path": d(&p2), "content": "#!/bin/sh\nexit 1\n" }),
        ),
    );
    assert_protected_deny(
        "rm <hw>/wtlink/.githooks/pre-push (wtlink -> main tree)",
        &f.bash(&format!("rm {}", d(&p2))),
    );
}

/// A `.harness-worktrees` that the session does not derive (not the sibling of
/// cwd / CLAUDE_PROJECT_DIR) is just a directory with that name.
#[test]
fn unrelated_harness_worktrees_dir_stays_denied() {
    let f = Fixture::new("n_unrelated");
    let p = f
        .base
        .join("other/.harness-worktrees/wt9/.githooks/pre-push");
    write(&p, "#!/bin/sh\nexit 0\n");
    assert_protected_deny(
        "Write <base>/other/.harness-worktrees/wt9/.githooks/pre-push",
        &f.tool(
            "Write",
            json!({ "file_path": d(&p), "content": "#!/bin/sh\nexit 1\n" }),
        ),
    );
    assert_protected_deny(
        "rm <base>/other/.harness-worktrees/wt9/.githooks/pre-push",
        &f.bash(&format!("rm {}", d(&p))),
    );
}

/// `<proj>/.harness-worktrees` (nested INSIDE the main tree) is not the root
/// derived from cwd = proj, which is `dirname(proj)/.harness-worktrees`.
#[test]
fn harness_worktrees_nested_in_main_tree_stays_denied() {
    let f = Fixture::new("n_nested");
    let p = f.proj.join(".harness-worktrees/x/.githooks/pre-push");
    write(&p, "#!/bin/sh\nexit 0\n");
    assert_protected_deny(
        "Write <proj>/.harness-worktrees/x/.githooks/pre-push",
        &f.tool(
            "Write",
            json!({ "file_path": d(&p), "content": "#!/bin/sh\nexit 1\n" }),
        ),
    );
    assert_protected_deny(
        "rm <proj>/.harness-worktrees/x/.githooks/pre-push",
        &f.bash(&format!("rm {}", d(&p))),
    );
}

/// The derived `.harness-worktrees` is itself a SYMLINK: scope.rs refuses it
/// as a root (`is_real_dir` is an lstat), so nothing below it is a worktree.
#[test]
fn symlinked_storage_root_stays_denied() {
    let f = Fixture::new("n_symroot");
    let proj2 = f.base.join("s2/proj2");
    std::fs::create_dir_all(&proj2).unwrap();
    let real = f.base.join("s2real");
    write(&real.join("wt/.githooks/pre-push"), "#!/bin/sh\nexit 0\n");
    std::os::unix::fs::symlink(&real, f.base.join("s2/.harness-worktrees")).unwrap();
    let p = f.base.join("s2/.harness-worktrees/wt/.githooks/pre-push");
    assert_protected_deny(
        "Write <s2>/.harness-worktrees(symlink)/wt/.githooks/pre-push",
        &f.run_tool(
            "Write",
            json!({ "file_path": d(&p), "content": "#!/bin/sh\nexit 1\n" }),
            &proj2,
            &proj2,
        ),
    );
    assert_protected_deny(
        "rm <s2>/.harness-worktrees(symlink)/wt/.githooks/pre-push",
        &f.run_tool(
            "Bash",
            json!({ "command": format!("rm {}", d(&p)) }),
            &proj2,
            &proj2,
        ),
    );
}

// ===========================================================================
// 4. Undetermined operands: never asserted Allow
// ===========================================================================

#[test]
fn undetermined_operands_are_not_asserted_allow() {
    let f = Fixture::new("u_undetermined");
    // `~` here expands to the FIXTURE home only if the hook expands it; the
    // contract is only that an unplaceable operand is not turned into Allow.
    assert_not_allow(
        "rm ~/.condukt/worktrees/run-x/.githooks/pre-commit",
        &f.bash("rm ~/.condukt/worktrees/run-x/.githooks/pre-commit"),
    );
    assert_not_allow(
        "rm <wt>/.githooks/pre-*",
        &f.bash(&format!("rm {}/.githooks/pre-*", d(&f.wt))),
    );
    assert_not_allow(
        "rm $WT/.githooks/pre-push",
        &f.bash("rm $WT/.githooks/pre-push"),
    );
}
