// このファイルは丸ごと integration test なので unwrap/expect を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Recursive `rm` with literal operands is judged by WHAT it destroys
//! (backlog 3aa215e1 slice 1; user ruling 2026-09-30: deleting is fine unless it
//! destroys system configuration or irreplaceable assets; anything that can be
//! recovered or redone may be deleted).
//!
//! Allow classes: temp roots (`/tmp`, `$TMPDIR`), cache roots
//! (`$HOME/.cache`, `$HOME/Library/Caches`), gitignored well-known build output
//! (`target`, `node_modules`, ...), and a git-tracked path whose every byte is
//! in HEAD. Everything else is Deny, and "cannot determine" is never Allow.
//!
//! Black-box: each case drives the real `blastguard` binary with a PreToolUse
//! Bash payload and reads the hook stdout (empty = Allow).
//!
//! Fixtures live under `/var/tmp` (canonicalised): it is neither `/tmp` nor the
//! hook's `TMPDIR` (removed unless a test sets it), nor inside a git work tree,
//! so a Deny is attributable to the rule under test and not to the harness
//! checkout's own `target/`.

use std::io::Write;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Fx {
    base: PathBuf,
    home: PathBuf,
    tmpdir: PathBuf,
    repo: PathBuf,
}

impl Fx {
    fn new(name: &str) -> Fx {
        let root =
            Path::new("/var/tmp").join(format!("bg-rm-by-target-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let base = std::fs::canonicalize(&root).unwrap();
        let f = Fx {
            home: base.join("home"),
            tmpdir: base.join("tmpdir"),
            repo: base.join("proj"),
            base,
        };
        for d in [
            f.home.join(".cache/app/sub"),
            f.home.join("Library/Caches/app/sub"),
            f.home.join(".ssh"),
            f.home.join("src/work"),
            f.tmpdir.join("scratch/sub"),
            f.repo.clone(),
        ] {
            std::fs::create_dir_all(d).unwrap();
        }
        for p in [
            ".cache/app/sub/f",
            "Library/Caches/app/sub/f",
            ".ssh/id",
            "src/work/f",
        ] {
            std::fs::write(f.home.join(p), "x").unwrap();
        }
        std::fs::write(f.tmpdir.join("scratch/sub/f"), "x").unwrap();
        f
    }

    fn h(&self) -> String {
        self.home.display().to_string()
    }

    fn git(&self, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.repo)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", &self.home)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// `git init` + write `files` (path, content) + `.gitignore` + one commit.
    fn init_repo(&self, gitignore: &str, files: &[(&str, &str)]) {
        self.git(&["init", "-q", "-b", "main"]);
        std::fs::write(self.repo.join(".gitignore"), gitignore).unwrap();
        for (p, c) in files {
            self.write(p, c);
        }
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", "init"]);
    }

    fn write(&self, rel: &str, content: &str) {
        let p = self.repo.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    fn r(&self, rel: &str) -> String {
        self.repo.join(rel).display().to_string()
    }

    /// Hook stdout; cwd = repo, project dir = repo, no TMPDIR.
    fn run(&self, command: &str) -> String {
        self.run_with(command, &self.repo, None)
    }

    fn run_tmp(&self, command: &str) -> String {
        self.run_with(command, &self.repo, Some(&self.tmpdir))
    }

    fn run_with(&self, command: &str, cwd: &Path, tmpdir: Option<&Path>) -> String {
        let payload = serde_json::json!({
            "session_id": "rm-by-target",
            "cwd": cwd.display().to_string(),
            "tool_name": "Bash",
            "tool_input": { "command": command },
        })
        .to_string();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("CLAUDE_PROJECT_DIR", &self.repo)
            .env("BLASTGUARD_APPROVALS_DIR", self.base.join("store"))
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_ENTRYPOINT", "cli")
            .env_remove("BLASTGUARD_ASK")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .current_dir(&self.base);
        match tmpdir {
            Some(t) => cmd.env("TMPDIR", t),
            None => cmd.env_remove("TMPDIR"),
        };
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
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
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
        "{case}: must be Allow (no output); got {out}"
    );
}
#[track_caller]
fn assert_deny(case: &str, out: &str) {
    assert!(is_deny(out), "{case}: must be Deny; got {out:?}");
}
#[track_caller]
fn assert_not_allow(case: &str, out: &str) {
    assert!(
        !is_allow(out),
        "{case}: must NOT be Allow; got empty output"
    );
}

const BUILD_IGNORE: &str = "target/\nnode_modules/\nsecrets/\n";

// ---- ALLOW: temp roots ------------------------------------------------------

#[test]
fn allow_rm_rf_strictly_inside_real_slash_tmp() {
    let f = Fx::new("real_tmp");
    let d = PathBuf::from(format!(
        "/tmp/bg-rm-by-target-{}-real_tmp",
        std::process::id()
    ));
    std::fs::create_dir_all(d.join("sub")).unwrap();
    std::fs::write(d.join("sub/f"), "x").unwrap();
    let out = f.run(&format!("rm -rf {}", d.display()));
    let _ = std::fs::remove_dir_all(&d);
    assert_allow("rm -rf /tmp/<unique dir>", &out);
}

#[test]
fn allow_rm_rf_strictly_inside_tmpdir_env() {
    let f = Fx::new("tmpdir");
    let out = f.run_tmp(&format!("rm -rf {}/scratch/sub", f.tmpdir.display()));
    assert_allow("rm -rf $TMPDIR/scratch/sub", &out);
}

// ---- ALLOW: cache roots -----------------------------------------------------

#[test]
fn allow_rm_rf_strictly_inside_home_dot_cache() {
    let f = Fx::new("dot_cache");
    let out = f.run(&format!("rm -rf {}/.cache/app", f.h()));
    assert_allow("rm -rf $HOME/.cache/app", &out);
}

#[test]
fn allow_rm_rf_strictly_inside_home_library_caches() {
    let f = Fx::new("lib_caches");
    let out = f.run(&format!("rm -rf {}/Library/Caches/app", f.h()));
    assert_allow("rm -rf $HOME/Library/Caches/app", &out);
}

// ---- ALLOW: gitignored build output ----------------------------------------

#[test]
fn allow_gitignored_target_and_node_modules() {
    let f = Fx::new("build_out");
    f.init_repo(BUILD_IGNORE, &[("src/lib.rs", "fn main(){}\n")]);
    f.write("target/debug/bin", "x");
    f.write("node_modules/pkg/index.js", "x");
    assert_allow(
        "rm -rf <repo>/target",
        &f.run(&format!("rm -rf {}", f.r("target"))),
    );
    assert_allow(
        "rm -rf <repo>/node_modules",
        &f.run(&format!("rm -rf {}", f.r("node_modules"))),
    );
    assert_allow(
        "rm -rf <repo>/target/debug (inside ignored build dir)",
        &f.run(&format!("rm -rf {}", f.r("target/debug"))),
    );
}

// ---- ALLOW: tracked, clean, fully committed ---------------------------------

#[test]
fn allow_tracked_clean_fully_committed_subdirectory() {
    let f = Fx::new("clean_tracked");
    f.init_repo(
        BUILD_IGNORE,
        &[
            ("src/lib.rs", "x\n"),
            ("docs/a.md", "a\n"),
            ("docs/sub/b.md", "b\n"),
        ],
    );
    let out = f.run(&format!("rm -rf {}", f.r("docs")));
    assert_allow("rm -rf <repo>/docs (every byte in HEAD)", &out);
}

#[test]
fn allow_tracked_clean_dir_named_target_via_recoverable_class() {
    // Name alone is irrelevant; a fully committed `target` dir is recoverable
    // from HEAD (spec class 5), so it is Allow by that class, not by class 4.
    let f = Fx::new("clean_tracked_target");
    f.init_repo("", &[("target/keep.txt", "k\n"), ("src/lib.rs", "x\n")]);
    let out = f.run(&format!("rm -rf {}", f.r("target")));
    assert_allow("rm -rf <repo>/target (tracked, clean)", &out);
}

// ---- DENY: roots themselves -------------------------------------------------

#[test]
fn deny_slash_tmp_root_itself() {
    let f = Fx::new("root_tmp");
    assert_deny("rm -rf /tmp", &f.run("rm -rf /tmp"));
    assert_deny("rm -rf /tmp/", &f.run("rm -rf /tmp/"));
}

#[test]
fn deny_tmpdir_root_itself() {
    let f = Fx::new("root_tmpdir");
    let out = f.run_tmp(&format!("rm -rf {}", f.tmpdir.display()));
    assert_deny("rm -rf $TMPDIR", &out);
}

#[test]
fn deny_cache_roots_themselves() {
    let f = Fx::new("root_cache");
    assert_deny(
        "rm -rf $HOME/.cache",
        &f.run(&format!("rm -rf {}/.cache", f.h())),
    );
    assert_deny(
        "rm -rf $HOME/Library/Caches",
        &f.run(&format!("rm -rf {}/Library/Caches", f.h())),
    );
}

#[test]
fn deny_project_root_itself() {
    let f = Fx::new("root_project");
    f.init_repo(BUILD_IGNORE, &[("src/lib.rs", "x\n")]);
    assert_deny(
        "rm -rf <repo>",
        &f.run(&format!("rm -rf {}", f.repo.display())),
    );
    assert_deny(
        "rm -rf <repo>/",
        &f.run(&format!("rm -rf {}/", f.repo.display())),
    );
}

// ---- DENY: home-level ------------------------------------------------------

#[test]
fn deny_home_itself() {
    let f = Fx::new("home_itself");
    assert_deny("rm -rf $HOME", &f.run(&format!("rm -rf {}", f.h())));
}

#[test]
fn deny_home_level_config_dir() {
    let f = Fx::new("home_ssh");
    assert_deny(
        "rm -rf $HOME/.ssh",
        &f.run(&format!("rm -rf {}/.ssh", f.h())),
    );
}

// ---- DENY: unrecoverable project content ------------------------------------

#[test]
fn deny_tracked_dir_with_modified_file() {
    let f = Fx::new("dirty_modified");
    f.init_repo(BUILD_IGNORE, &[("docs/a.md", "a\n"), ("src/lib.rs", "x\n")]);
    f.write("docs/a.md", "edited, not committed\n");
    assert_deny(
        "rm -rf <repo>/docs with a modified tracked file",
        &f.run(&format!("rm -rf {}", f.r("docs"))),
    );
}

#[test]
fn deny_tracked_dir_with_untracked_file() {
    let f = Fx::new("dirty_untracked");
    f.init_repo(BUILD_IGNORE, &[("docs/a.md", "a\n"), ("src/lib.rs", "x\n")]);
    f.write("docs/new-notes.md", "never committed\n");
    assert_deny(
        "rm -rf <repo>/docs with an untracked file",
        &f.run(&format!("rm -rf {}", f.r("docs"))),
    );
}

#[test]
fn deny_tracked_dir_with_staged_uncommitted_file() {
    let f = Fx::new("dirty_staged");
    f.init_repo(BUILD_IGNORE, &[("docs/a.md", "a\n"), ("src/lib.rs", "x\n")]);
    f.write("docs/staged.md", "staged only\n");
    f.git(&["add", "docs/staged.md"]);
    assert_deny(
        "rm -rf <repo>/docs with a staged-not-committed file",
        &f.run(&format!("rm -rf {}", f.r("docs"))),
    );
}

#[test]
fn deny_gitignored_secrets_dir_not_named_like_build_output() {
    let f = Fx::new("ignored_secrets");
    f.init_repo(BUILD_IGNORE, &[("src/lib.rs", "x\n")]);
    f.write("secrets/api.key", "irreplaceable\n");
    assert_deny(
        "rm -rf <repo>/secrets (ignored, not build-output name)",
        &f.run(&format!("rm -rf {}", f.r("secrets"))),
    );
}

#[test]
fn deny_tracked_not_ignored_target_with_untracked_content() {
    // Name `target` alone must not grant Allow: git must also report it ignored.
    let f = Fx::new("tracked_target_dirty");
    f.init_repo("", &[("target/keep.txt", "k\n"), ("src/lib.rs", "x\n")]);
    f.write("target/precious.txt", "untracked and not ignored\n");
    assert_deny(
        "rm -rf <repo>/target (name matches, not ignored, dirty)",
        &f.run(&format!("rm -rf {}", f.r("target"))),
    );
}

#[test]
fn deny_tracked_dir_containing_ignored_non_build_content() {
    let f = Fx::new("tracked_with_ignored");
    f.init_repo("*.env\n", &[("app/main.rs", "x\n")]);
    f.write("app/prod.env", "SECRET=1\n");
    assert_deny(
        "rm -rf <repo>/app containing an ignored .env",
        &f.run(&format!("rm -rf {}", f.r("app"))),
    );
}

// ---- DENY: mixed / escapes / undetermined -----------------------------------

#[test]
fn deny_mixed_one_allowed_one_denied_operand() {
    let f = Fx::new("mixed");
    let out = f.run(&format!("rm -rf {}/.cache/app {}/.ssh", f.h(), f.h()));
    assert_deny("allowed cache operand + $HOME/.ssh", &out);
}

#[test]
fn not_allow_symlink_in_temp_root_pointing_at_home() {
    let f = Fx::new("sym_tmp_home");
    symlink(&f.home, f.tmpdir.join("x")).unwrap();
    let out = f.run_tmp(&format!("rm -rf {}/x/", f.tmpdir.display()));
    assert_not_allow("rm -rf $TMPDIR/x/ where x -> fake HOME", &out);
    let out = f.run_tmp(&format!("rm -rf {}/x/src", f.tmpdir.display()));
    assert_not_allow("rm -rf $TMPDIR/x/src where x -> fake HOME", &out);
}

#[test]
fn not_allow_symlinked_target_pointing_at_parent() {
    let f = Fx::new("sym_target");
    f.init_repo("target\n", &[("src/lib.rs", "x\n")]);
    symlink("..", f.repo.join("target")).unwrap();
    let out = f.run(&format!("rm -rf {}/", f.r("target")));
    assert_not_allow("rm -rf <repo>/target/ where target -> ..", &out);
    let out = f.run(&format!("rm -rf {}", f.r("target")));
    assert_not_allow("rm -rf <repo>/target where target -> ..", &out);
}

#[test]
fn not_allow_dotdot_escape_out_of_temp_root() {
    let f = Fx::new("dotdot_tmp");
    let out = f.run_tmp(&format!("rm -rf {}/../home/src", f.tmpdir.display()));
    assert_not_allow("rm -rf $TMPDIR/../home/src", &out);
}

#[test]
fn not_allow_dotdot_escape_out_of_ignored_target() {
    let f = Fx::new("dotdot_target");
    f.init_repo(BUILD_IGNORE, &[("src/lib.rs", "x\n")]);
    f.write("target/debug/bin", "x");
    let out = f.run("rm -rf target/../src");
    assert_not_allow("rm -rf target/../src", &out);
    let out = f.run(&format!("rm -rf {}/target/../src", f.repo.display()));
    assert_not_allow("rm -rf <repo>/target/../src", &out);
}

#[test]
fn not_allow_when_non_rm_segment_precedes_the_rm() {
    let f = Fx::new("multi_seg");
    let out = f.run_tmp(&format!(
        "mv {}/src/work {}/x && rm -rf {}/x",
        f.h(),
        f.tmpdir.display(),
        f.tmpdir.display()
    ));
    assert_not_allow("mv A $TMPDIR/x && rm -rf $TMPDIR/x", &out);
}

#[test]
fn not_allow_glob_operand_in_temp_root() {
    let f = Fx::new("glob");
    let out = f.run_tmp(&format!("rm -rf {}/scratch/*", f.tmpdir.display()));
    assert_not_allow("rm -rf $TMPDIR/scratch/* (wildcard is out of scope)", &out);
}

#[test]
fn not_allow_nonexistent_operand_cannot_be_judged() {
    let f = Fx::new("unresolvable");
    f.init_repo(BUILD_IGNORE, &[("src/lib.rs", "x\n")]);
    let out = f.run(&format!("rm -rf {}", f.r("no/such/dir")));
    assert_not_allow("rm -rf <repo>/no/such/dir (unresolvable)", &out);
}

#[test]
fn not_allow_system_dirs() {
    let f = Fx::new("system");
    for d in ["/", "/etc", "/usr", "/Library", "/var"] {
        assert_not_allow(&format!("rm -rf {d}"), &f.run(&format!("rm -rf {d}")));
    }
}

// ---- controls: protected precedence still runs first -------------------------

#[test]
fn control_protected_dot_git_still_denied() {
    let f = Fx::new("ctl_git");
    f.init_repo(BUILD_IGNORE, &[("src/lib.rs", "x\n")]);
    assert_deny(
        "rm -rf <repo>/.git",
        &f.run(&format!("rm -rf {}", f.r(".git"))),
    );
}

#[test]
fn control_protected_dot_claude_still_denied() {
    let f = Fx::new("ctl_claude");
    f.init_repo(
        BUILD_IGNORE,
        &[(".claude/settings.json", "{}\n"), ("src/lib.rs", "x\n")],
    );
    assert_deny(
        "rm -rf <repo>/.claude (tracked and clean, still protected)",
        &f.run(&format!("rm -rf {}", f.r(".claude"))),
    );
}
