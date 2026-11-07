#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Backlog 78cee053. User ruling (2026-11-07, "Re-run from project root"):
//! `next --claim` re-runs the top candidate's stored repro in the TICKET'S
//! PROJECT ROOT (the task row's `project` field), not in the process cwd. If
//! that root is not a git checkout the claim is refused (non-zero) with the
//! reason printed (CLAUDE.md §3: undetermined resolves to the restrictive side).
//!
//! Setup: the claim runs from a NON-git cwd (an orphan dir, premise asserted)
//! against a pinned store (`store_dir` in `$HOME/.backlog/config.toml`, the
//! `cli_contract.rs` pattern) holding one pending row whose `project` is the
//! root under test and whose stored repro is `bash tests/repro_yes.sh` (exit 1
//! = reproduced, see `evidence.rs`).

mod common;

use common::Out;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const ID: &str = "root0001";

fn orphan_dir(td: &tempfile::TempDir, name: &str) -> PathBuf {
    let d = td.path().join(name);
    std::fs::create_dir_all(&d).unwrap();
    let d = d.canonicalize().unwrap();
    let mut cur = Some(d.as_path());
    while let Some(c) = cur {
        assert!(
            !c.join(".git").exists(),
            "{} is inside a git repo; the test would be vacuous",
            d.display()
        );
        cur = c.parent();
    }
    d
}

/// A PRIMARY git checkout (its canonical project identity is its own path, so
/// the stored `project` matches the `--project` filter) with a committed,
/// clean-tree RED repro `tests/repro_yes.sh` (exit 1 = reproduced).
fn red_checkout(td: &tempfile::TempDir, name: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let d = td.path().join(name);
    std::fs::create_dir_all(d.join("tests")).unwrap();
    let d = d.canonicalize().unwrap();
    let script = d.join("tests/repro_yes.sh");
    std::fs::write(&script, "#!/bin/bash\necho 'bug present'\nexit 1\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let git = |args: &[&str]| {
        let out = harness_core::git_env::scrub(&mut Command::new("git"))
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .current_dir(&d)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["add", "-f", "--", "tests/repro_yes.sh"]);
    git(&["commit", "-q", "--no-verify", "-m", "red repro"]);
    d
}

fn pending_block(project: &str) -> String {
    format!(
        "[[task]]\nid = \"{ID}\"\ntitle = \"rerun in project root\"\nproject = \"{project}\"\n\
         tags = [\"p1\"]\nstatus = \"pending\"\nnotes = \"\"\n\
         created_at = 1000\nupdated_at = 1000\nweight = 0.0\n[task.repro]\n\
         outcome = \"reproduced\"\ncmd = \"bash tests/repro_yes.sh\"\nobserved_at = 1000\n\
         detail = \"fixture\"\n\n"
    )
}

/// Pin the store under `home` and seed it with one pending row for `project`.
fn seed_pinned_store(home: &Path, pinned: &Path, project: &str) {
    std::fs::create_dir_all(home.join(".backlog")).unwrap();
    std::fs::create_dir_all(pinned).unwrap();
    std::fs::write(
        home.join(".backlog/config.toml"),
        format!("store_dir = \"{}\"\n", pinned.display()),
    )
    .unwrap();
    std::fs::write(pinned.join("tasks.toml"), pending_block(project)).unwrap();
}

fn run_in(home: &Path, cwd: &Path, args: &[&str]) -> Out {
    let mut c = Command::new(env!("CARGO_BIN_EXE_backlog"));
    harness_core::git_env::scrub(&mut c);
    c.args(args)
        .env("HOME", home)
        .env("PATH", common::path_with_condukt_shim())
        .env_remove("BACKLOG_DISABLE")
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = c.spawn().unwrap().wait_with_output().unwrap();
    Out {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

#[test]
#[ignore = "backlog 78cee053: RED, remove ignore when fixed"]
fn next_claim_reruns_the_repro_in_the_ticket_project_root_not_the_cwd() {
    // The ticket's project: a real, clean git checkout with a committed RED repro.
    let td = tempfile::tempdir().unwrap();
    let home = td.path().join("home");
    let cwd = orphan_dir(&td, "orphan");
    let root = red_checkout(&td, "red-checkout");
    let proj = root.to_string_lossy().into_owned();
    seed_pinned_store(&home, &td.path().join("pinned"), &proj);

    // From a non-git cwd the default scope is undeterminable, so name it.
    let c = run_in(&home, &cwd, &["next", "--claim", "--project", &proj]);
    assert_eq!(
        c.code,
        0,
        "next --claim run from a non-git cwd must re-run the repro in the ticket's project \
         root ({}) and hand the still-RED ticket out: {}",
        proj,
        c.both()
    );
    assert!(
        c.stdout.contains(ID),
        "the still-RED ticket {ID} must be handed out: {}",
        c.both()
    );
}

#[test]
#[ignore = "backlog 78cee053: RED, remove ignore when fixed"]
fn next_claim_refuses_when_the_ticket_project_root_is_not_a_git_checkout() {
    let td = tempfile::tempdir().unwrap();
    let home = td.path().join("home");
    // The cwd is a clean git checkout with a committed RED repro, so a cwd-based
    // re-run would reproduce and hand the ticket out. Only a re-run in the
    // ticket's (non-git) project root can refuse. (With a non-git cwd this test
    // would pass vacuously, for the cwd-based reason.)
    let cwd = red_checkout(&td, "red-cwd");
    let root = orphan_dir(&td, "not-a-checkout");
    // Even a script that would exit 1 there must not count: no checkout, no evidence.
    std::fs::create_dir_all(root.join("tests")).unwrap();
    std::fs::write(root.join("tests/repro_yes.sh"), "#!/bin/bash\nexit 1\n").unwrap();
    seed_pinned_store(&home, &td.path().join("pinned"), &root.to_string_lossy());

    let proj = root.to_string_lossy().into_owned();
    let c = run_in(&home, &cwd, &["next", "--claim", "--project", &proj]);
    assert_ne!(
        c.code,
        0,
        "next --claim must exit non-zero when the project root is not a git checkout: {}",
        c.both()
    );
    assert!(
        !c.stdout.contains(ID),
        "{ID} was handed out although its project root is not a git checkout: {}",
        c.both()
    );
    let msg = c.both().to_lowercase();
    assert!(
        msg.contains("could not be determined")
            || msg.contains("git checkout")
            || msg.contains("not a git"),
        "the refusal must print a reason (repro undetermined / no git checkout): {}",
        c.both()
    );
    let claimed = std::fs::read_to_string(td.path().join("pinned/tasks.toml")).unwrap();
    assert!(
        claimed.contains("status = \"pending\""),
        "{ID} must stay pending after a refused claim: {claimed}"
    );
}
