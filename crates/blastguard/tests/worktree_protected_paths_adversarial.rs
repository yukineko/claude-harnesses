// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Adversarial companion to `worktree_protected_paths.rs`, written by an
//! independent verifier (not the implementer of 6316fec1 / blastguard 0.2.106).
//!
//! Every test pairs its "must stay refused" case with a POSITIVE control in the
//! same fixture (the same verb on a real linked checkout is Allow), so a binary
//! that simply denies everything — the pre-0.2.106 behaviour — fails here
//! instead of passing vacuously.
//!
//! Covered here and not by the implementer's file:
//!   * `rm -rf <checkout>/.githooks` (the `worktree_confined` change) is Allow,
//!     `rm -rf <main>/.githooks` stays Deny, and a mixed operand list stays Deny;
//!   * a compound line that swaps a symlink into the checkout before writing;
//!   * a relative operand from a session whose cwd is the checkout;
//!   * a storage-root entry whose `.git` is a directory, or a `.git` file that
//!     does not start with `gitdir:`;
//!   * a checkout entry that is itself a symlink (to a real linked checkout);
//!   * a leaf symlink inside the checkout pointing into the main tree;
//!   * paths below a `.git` component inside the checkout;
//!   * an unreadable checkout `.git` file (Undetermined probe) is not Allow.

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
            .join("worktree_protected_paths_adversarial")
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
            "session_id": "worktree-protected-paths-adv",
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

fn write_input(p: &Path) -> Value {
    json!({ "file_path": d(p), "content": "#!/bin/sh\nexit 1\n" })
}

/// Positive control used by every negative test below: the same shape on the
/// real linked checkout `wt1` is Allow. On a binary without the 0.2.106
/// exemption this fails, so the negative assertions cannot pass vacuously.
#[track_caller]
fn control_allow(f: &Fixture) {
    assert_allow(
        "[control] Write <wt>/.githooks/pre-push",
        &f.tool("Write", write_input(&f.wt.join(".githooks/pre-push"))),
    );
}

/// A storage-root entry with protected-named files, built WITHOUT `git
/// worktree add`, so the test controls what its `.git` is.
fn fake_entry(f: &Fixture, name: &str) -> PathBuf {
    let e = f.hw.join(name);
    write(&e.join(".githooks/pre-push"), "#!/bin/sh\nexit 0\n");
    write(&e.join(".claude/settings.json"), "{}\n");
    e
}

// ===========================================================================
// 1. rm -rf of a protected DIRECTORY (worktree_confined path)
// ===========================================================================

#[test]
fn adv_rm_rf_githooks_dir_in_real_checkout_is_allowed_main_denied() {
    let f = Fixture::new("a_rmrf_dir");
    pair(
        "rm -rf <tree>/.githooks",
        &f.bash(&format!("rm -rf {}", d(&f.proj.join(".githooks")))),
        &f.bash(&format!("rm -rf {}", d(&f.wt.join(".githooks")))),
    );
    pair(
        "rm -rf <tree>/.claude",
        &f.bash(&format!("rm -rf {}", d(&f.proj.join(".claude")))),
        &f.bash(&format!("rm -rf {}", d(&f.wt.join(".claude")))),
    );
    // condukt storage root as well.
    assert_allow(
        "rm -rf <condukt wt>/.githooks",
        &f.bash(&format!("rm -rf {}", d(&f.cwt.join(".githooks")))),
    );
    // From a session whose cwd is the checkout itself.
    assert_allow(
        "rm -rf <wt>/.githooks (session in wt)",
        &f.tool_from_wt(
            "Bash",
            json!({ "command": format!("rm -rf {}", d(&f.wt.join(".githooks"))) }),
        ),
    );
}

#[test]
fn adv_rm_rf_mixed_operands_checkout_and_main_stays_denied() {
    let f = Fixture::new("a_rmrf_mixed");
    assert_allow(
        "[control] rm -rf <wt>/.githooks",
        &f.bash(&format!("rm -rf {}", d(&f.wt.join(".githooks")))),
    );
    assert_deny(
        "rm -rf <wt>/.githooks <main>/.githooks",
        &f.bash(&format!(
            "rm -rf {} {}",
            d(&f.wt.join(".githooks")),
            d(&f.proj.join(".githooks"))
        )),
    );
    assert_deny(
        "rm -rf <main>/.githooks <wt>/.githooks",
        &f.bash(&format!(
            "rm -rf {} {}",
            d(&f.proj.join(".githooks")),
            d(&f.wt.join(".githooks"))
        )),
    );
}

// ===========================================================================
// 2. Adversarial placements
// ===========================================================================

/// The swap: judge sees a real `<wt>/.githooks`, but by the time the redirect
/// runs it is a symlink into the main tree.
#[test]
fn adv_compound_symlink_swap_then_write_is_denied() {
    let f = Fixture::new("a_swap");
    let wt_hooks = d(&f.wt.join(".githooks"));
    let main_hooks = d(&f.proj.join(".githooks"));
    let target = d(&f.wt.join(".githooks/pre-push"));
    assert_allow(
        "[control] echo x > <wt>/.githooks/pre-push",
        &f.bash(&format!("echo x > {target}")),
    );
    for sep in [" && ", "; ", "\n", " || ", " & "] {
        let cmd = format!(
            "mv {wt_hooks} {wt_hooks}.old{sep}ln -s {main_hooks} {wt_hooks}{sep}echo x > {target}"
        );
        assert_not_allow(&format!("swap with sep {sep:?}: {cmd}"), &f.bash(&cmd));
    }
    // Same swap, final write via tee / cp instead of a redirect.
    let cmd = format!(
        "mv {wt_hooks} {wt_hooks}.old && ln -s {main_hooks} {wt_hooks} && cp /dev/null {target}"
    );
    assert_not_allow(&cmd, &f.bash(&cmd));
    let cmd =
        format!("mv {wt_hooks} {wt_hooks}.old && ln -s {main_hooks} {wt_hooks} && rm -rf {target}");
    assert_not_allow(&cmd, &f.bash(&cmd));
    // The fixture must be untouched (the hook judges, it does not run).
    assert!(f.wt.join(".githooks").is_dir());
    assert!(!f.wt.join(".githooks").is_symlink());
}

/// Relative operands are judged against the analysis' model of the cwd, and
/// the design refuses to lift the rule for them. Session cwd = the checkout.
#[test]
fn adv_relative_operand_from_worktree_cwd_stays_refused() {
    let f = Fixture::new("a_relative");
    let abs = d(&f.wt.join(".githooks/pre-push"));
    let bash_wt = |c: &str| f.tool_from_wt("Bash", json!({ "command": c }));
    assert_allow(
        "[control] rm <abs wt>/.githooks/pre-push",
        &bash_wt(&format!("rm {abs}")),
    );
    assert_not_allow(
        "rm .githooks/pre-push (cwd wt)",
        &bash_wt("rm .githooks/pre-push"),
    );
    assert_not_allow("rm -rf .githooks (cwd wt)", &bash_wt("rm -rf .githooks"));
    assert_not_allow(
        "echo x > .githooks/pre-push (cwd wt)",
        &bash_wt("echo x > .githooks/pre-push"),
    );
    assert_not_allow(
        "rm ./.claude/settings.json (cwd wt)",
        &bash_wt("rm ./.claude/settings.json"),
    );
}

/// A storage-root entry that is a MAIN checkout (`.git` is a directory).
#[test]
fn adv_entry_with_dotgit_directory_stays_denied() {
    let f = Fixture::new("a_dotgit_dir");
    control_allow(&f);
    let e = fake_entry(&f, "mainlike");
    git(&e, &["init", "-q"]);
    assert!(e.join(".git").is_dir());
    let p = e.join(".githooks/pre-push");
    assert_protected_deny(
        "Write <hw>/mainlike/.githooks/pre-push",
        &f.tool("Write", write_input(&p)),
    );
    assert_protected_deny(
        "rm <hw>/mainlike/.githooks/pre-push",
        &f.bash(&format!("rm {}", d(&p))),
    );
    assert_deny(
        "rm -rf <hw>/mainlike/.githooks",
        &f.bash(&format!("rm -rf {}", d(&e.join(".githooks")))),
    );
}

/// `.git` FILE whose content is not `gitdir:` (and one that is too short).
#[test]
fn adv_entry_with_non_gitdir_dotgit_file_stays_denied() {
    let f = Fixture::new("a_dotgit_bogus");
    control_allow(&f);
    for (name, content) in [
        ("bogus", "nope: /somewhere\n"),
        ("short", "gitd"),
        ("empty", ""),
        ("upper", "GITDIR: /x\n"),
        ("leading_space", " gitdir: /x\n"),
    ] {
        let e = fake_entry(&f, name);
        std::fs::write(e.join(".git"), content).unwrap();
        let p = e.join(".githooks/pre-push");
        assert_protected_deny(
            &format!("Write <hw>/{name}/.githooks/pre-push (.git = {content:?})"),
            &f.tool("Write", write_input(&p)),
        );
        assert_deny(
            &format!("rm -rf <hw>/{name}/.githooks (.git = {content:?})"),
            &f.bash(&format!("rm -rf {}", d(&e.join(".githooks")))),
        );
    }
    // No `.git` at all.
    let e = fake_entry(&f, "nogit");
    assert_protected_deny(
        "Write <hw>/nogit/.githooks/pre-push",
        &f.tool("Write", write_input(&e.join(".githooks/pre-push"))),
    );
}

/// The checkout ENTRY is a symlink. The scope.rs doc says "a checkout entry
/// that is itself a symlink, is never `Inside`" — pinned here for a symlink
/// that points at a REAL linked checkout (the implementer's test only covers a
/// symlink to the main tree), and for a symlink spelled from outside the
/// storage root.
#[test]
fn adv_symlinked_checkout_entry_is_not_inside() {
    let f = Fixture::new("a_entry_symlink");
    control_allow(&f);
    let link = f.hw.join("wt1link");
    std::os::unix::fs::symlink(&f.wt, &link).unwrap();
    let p = link.join(".githooks/pre-push");
    assert_protected_deny(
        "Write <hw>/wt1link/.githooks/pre-push (wt1link -> wt1)",
        &f.tool("Write", write_input(&p)),
    );
    assert_protected_deny(
        "rm <hw>/wt1link/.githooks/pre-push (wt1link -> wt1)",
        &f.bash(&format!("rm {}", d(&p))),
    );
    let outside = f.base.join("elsewhere-link");
    std::os::unix::fs::symlink(&f.wt, &outside).unwrap();
    let p2 = outside.join(".githooks/pre-push");
    assert_protected_deny(
        "Write <base>/elsewhere-link/.githooks/pre-push (-> wt1)",
        &f.tool("Write", write_input(&p2)),
    );
}

/// A checkout under the storage root that is a symlink to a linked worktree
/// living OUTSIDE every storage root: its real path is not under a root.
#[test]
fn adv_symlinked_checkout_entry_to_outside_worktree_stays_denied() {
    let f = Fixture::new("a_entry_symlink_out");
    control_allow(&f);
    let outside_wt = f.base.join("outside-wt");
    git(
        &f.proj,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "outside",
            outside_wt.to_str().unwrap(),
        ],
    );
    let link = f.hw.join("outlink");
    std::os::unix::fs::symlink(&outside_wt, &link).unwrap();
    let p = link.join(".githooks/pre-push");
    assert_protected_deny(
        "Write <hw>/outlink/.githooks/pre-push (-> worktree outside roots)",
        &f.tool("Write", write_input(&p)),
    );
    assert_protected_deny(
        "Write <outside-wt>/.githooks/pre-push (worktree outside roots)",
        &f.tool("Write", write_input(&outside_wt.join(".githooks/pre-push"))),
    );
}

/// A LEAF symlink: `<wt>/.githooks/pre-push` itself points at the main
/// tree's file (the implementer's test symlinks the directory, not the leaf).
#[test]
fn adv_leaf_symlink_into_main_tree_stays_denied() {
    let f = Fixture::new("a_leaf_symlink");
    control_allow(&f);
    let leaf = f.wt.join(".githooks/pre-commit");
    std::fs::remove_file(&leaf).unwrap();
    std::os::unix::fs::symlink(f.proj.join(".githooks/pre-commit"), &leaf).unwrap();
    assert_protected_deny(
        "Write <wt>/.githooks/pre-commit (leaf -> main)",
        &f.tool("Write", write_input(&leaf)),
    );
    assert_protected_deny(
        "Edit <wt>/.githooks/pre-commit (leaf -> main)",
        &f.tool(
            "Edit",
            json!({ "file_path": d(&leaf), "old_string": "exit 0", "new_string": "exit 1" }),
        ),
    );
    assert_deny(
        "echo x > <wt>/.githooks/pre-commit (leaf -> main)",
        &f.bash(&format!("echo x > {}", d(&leaf))),
    );
    assert_deny(
        "sed -i <wt>/.githooks/pre-commit (leaf -> main)",
        &f.bash(&format!("sed -i 's/0/1/' {}", d(&leaf))),
    );
}

/// Anything below a `.git` component inside the checkout is git metadata (a
/// nested repo, or the checkout's own `.git` file), never lifted.
#[test]
fn adv_paths_below_dotgit_inside_checkout_stay_denied() {
    let f = Fixture::new("a_below_dotgit");
    control_allow(&f);
    let nested = f.wt.join("vendor/inner");
    std::fs::create_dir_all(&nested).unwrap();
    git(&nested, &["init", "-q"]);
    for rel in [
        "vendor/inner/.git/hooks/pre-commit",
        "vendor/inner/.git/config",
    ] {
        let p = f.wt.join(rel);
        assert_deny(
            &format!("Write <wt>/{rel}"),
            &f.tool("Write", write_input(&p)),
        );
        assert_deny(
            &format!("echo x > <wt>/{rel}"),
            &f.bash(&format!("echo x > {}", d(&p))),
        );
    }
    assert_deny(
        "rm -rf <wt>/vendor/inner/.git",
        &f.bash(&format!("rm -rf {}", d(&nested.join(".git")))),
    );
}

/// The checkout's own `.git` FILE (not "below" `.git`, so outside the
/// 6316fec1 contract). Kept separate because it is measured Allow on the BASE
/// binary too (pre-existing; non-recursive single-file rm is below the
/// destructive bar), so a failure here is not attributable to 0.2.106.
#[test]
#[ignore = "backlog eba15868: open defect, remove ignore when fixed"]
fn adv_checkout_own_dotgit_file_is_not_allowed() {
    let f = Fixture::new("a_own_dotgit");
    control_allow(&f);
    let dotgit = f.wt.join(".git");
    let rm = f.bash(&format!("rm {}", d(&dotgit)));
    let rm_f = f.bash(&format!("rm -f {}", d(&dotgit)));
    let w = f.tool(
        "Write",
        json!({ "file_path": d(&dotgit), "content": "gitdir: /x\n" }),
    );
    let redir = f.bash(&format!("echo gitdir: /x > {}", d(&dotgit)));
    let results = [
        ("rm <wt>/.git", is_deny(&rm)),
        ("rm -f <wt>/.git", is_deny(&rm_f)),
        ("Write <wt>/.git", is_deny(&w)),
        ("echo > <wt>/.git", is_deny(&redir)),
    ];
    assert!(
        results.iter().all(|(_, denied)| *denied),
        "every write/delete of the checkout's own .git file must be Deny; \
         (case, denied) = {results:?}"
    );
}

// ===========================================================================
// 3. Undetermined probe
// ===========================================================================

/// The checkout's `.git` file is unreadable (EACCES): the probe cannot tell
/// whether this is a linked checkout, so the exemption must not apply.
#[test]
fn adv_unreadable_checkout_dotgit_is_not_allow() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new("a_unreadable");
    control_allow(&f);
    let wt3 = f.hw.join("wt3");
    git(
        &f.proj,
        &["worktree", "add", "-q", "-b", "wt3", wt3.to_str().unwrap()],
    );
    let dotgit = wt3.join(".git");
    let p = wt3.join(".githooks/pre-push");
    // Readable: Allow (proves wt3 is otherwise a qualifying checkout).
    assert_allow(
        "[control] Write <wt3>/.githooks/pre-push",
        &f.tool("Write", write_input(&p)),
    );
    std::fs::set_permissions(&dotgit, std::fs::Permissions::from_mode(0o000)).unwrap();
    // If the test runs as root the chmod observes nothing; fail loudly.
    assert!(
        std::fs::read(&dotgit).is_err(),
        "chmod 000 did not make <wt3>/.git unreadable (running as root?)"
    );
    let w = f.tool("Write", write_input(&p));
    let r = f.bash(&format!("rm {}", d(&p)));
    let rr = f.bash(&format!("rm -rf {}", d(&wt3.join(".githooks"))));
    let e = f.bash(&format!("echo x > {}", d(&p)));
    std::fs::set_permissions(&dotgit, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_not_allow("Write <wt3>/.githooks/pre-push (.git unreadable)", &w);
    assert_not_allow("rm <wt3>/.githooks/pre-push (.git unreadable)", &r);
    assert_not_allow("rm -rf <wt3>/.githooks (.git unreadable)", &rr);
    assert_not_allow("echo x > <wt3>/.githooks/pre-push (.git unreadable)", &e);
}
