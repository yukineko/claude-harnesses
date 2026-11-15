#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (audit 2026-11-13 K4): in a SHALLOW clone (`git clone --depth 1
//! file://...`) `git log HEAD` only sees the tip, so a landed fix commit
//! `(backlog <id>)` that is not the tip is invisible to
//! `evidence::fixed_task_ids`; `backlog next` treats the history as complete
//! and dispatches the already-fixed task. A truncated history is an
//! undetermined answer to "did the fix land", not "nothing landed".
//!
//! Fixture: the standard fixture repo with fix commit `(backlog X)` followed by
//! a later commit; clone at depth 1; the repo's `.backlog` store is copied into
//! the clone; `backlog next` runs in the clone. Accepted fixed behaviour: X is
//! not handed out -- either `next` returns Y, or it exits non-zero saying the
//! history is shallow/truncated. A non-ignored control runs the identical
//! clone + copied-store path with a FULL clone and requires Y (X skipped as
//! fixed). All git invocations use the fixture's isolated env (scrubbed git
//! env, temp HOME, no global/system config). Measurement point: 09dd0c8c,
//! 2026-11-13.
//!
//! Does NOT prove: `next --claim`, a deepen/unshallow remedy, or behaviour
//! when the fix commit IS the tip of the shallow clone.

mod common;

use std::path::Path;
use std::process::Command;

fn isolate(cmd: &mut Command, home: &Path) {
    harness_core::git_env::scrub(cmd);
    cmd.env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com");
}

fn git_in(home: &Path, dir: &Path, args: &[&str]) -> String {
    let mut cmd = Command::new("git");
    isolate(&mut cmd, home);
    let o = cmd.args(args).current_dir(dir).output().expect("git runs");
    assert!(
        o.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dst = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &dst);
        } else {
            std::fs::copy(e.path(), dst).unwrap();
        }
    }
}

fn setup() -> (common::Fixture, String, String) {
    let fx = common::Fixture::new("k4");
    let (ox, x) = fx.add_extra(
        "alpha parser drops trailing newline",
        &["--priority", "p0", "--repro-test", common::REPRO_YES],
    );
    let (oy, y) = fx.add_extra(
        "zulu renderer misaligns footer",
        &["--priority", "p2", "--repro-test", common::REPRO_YES],
    );
    let x = x.unwrap_or_else(|| panic!("setup: X not filed: {}", ox.both()));
    let y = y.unwrap_or_else(|| panic!("setup: Y not filed: {}", oy.both()));
    fx.write("src/fix.txt", "fixed\n");
    fx.commit_paths(
        &["src/fix.txt"],
        &format!("fix: alpha parser drops trailing newline (backlog {x})"),
    );
    fx.write("src/later.txt", "later\n");
    fx.commit_paths(&["src/later.txt"], "chore: later unrelated commit");
    (fx, x, y)
}

/// Clone the fixture repo (`depth` Some = shallow), copy the store in, run
/// `backlog next` there. Returns (exit code, stdout, stdout+stderr).
fn next_in_clone(fx: &common::Fixture, depth: Option<&str>) -> (i32, String, String) {
    let parent = common::unique_dir("k4-clone");
    let clone = parent.join("c");
    let url = format!("file://{}", fx.repo.display());
    let mut args = vec!["clone", "-q"];
    if let Some(d) = depth {
        args.extend(["--depth", d]);
    }
    args.extend([url.as_str(), clone.to_str().unwrap()]);
    git_in(&fx.home, &parent, &args);
    assert_eq!(
        git_in(&fx.home, &clone, &["rev-parse", "--is-shallow-repository"]),
        if depth.is_some() { "true" } else { "false" },
        "setup: clone shallow-ness"
    );
    copy_dir(&fx.repo.join(".backlog"), &clone.join(".backlog"));

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_backlog"));
    isolate(&mut cmd, &fx.home);
    let out = cmd
        .arg("next")
        .current_dir(&clone)
        .output()
        .expect("binary runs");
    let code = out.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let both = format!("{stdout}{}", String::from_utf8_lossy(&out.stderr));
    (code, stdout, both)
}

#[test]
fn control_full_clone_with_copied_store_skips_fixed_task() {
    let (fx, x, y) = setup();
    let (code, stdout, both) = next_in_clone(&fx, None);
    assert_eq!(code, 0, "control: full clone must hand out Y: {both}");
    assert!(
        stdout.contains(&y) && !stdout.contains(&x),
        "control: full clone must hand out Y ({y}) and skip fixed X ({x}): {stdout}"
    );
}

#[test]
#[ignore = "RED: K4 — remove the ignore when fixed"]
fn next_in_shallow_clone_does_not_dispatch_fixed_task() {
    let (fx, x, y) = setup();
    let (code, stdout, both) = next_in_clone(&fx, Some("1"));
    if code == 0 {
        assert!(
            !stdout.contains(&x),
            "`next` in a shallow clone handed out X ({x}) although its fix commit `(backlog {x})` \
             exists in the full history; expected Y ({y}) or a refusal. stdout: {stdout}"
        );
        assert!(
            stdout.contains(&y),
            "`next` exited 0 without X but did not hand out Y ({y}). stdout: {stdout}"
        );
    } else {
        let low = both.to_lowercase();
        assert!(
            low.contains("shallow") || low.contains("truncated"),
            "`next` exited {code} but did not say the history is shallow/truncated: {both}"
        );
    }
}
