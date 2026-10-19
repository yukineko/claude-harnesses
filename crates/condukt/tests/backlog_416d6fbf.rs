// Integration test: unwrap/expect/panic are allowed (the workspace lint denies
// them for production code only).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog 416d6fbf: `RunState::save` (crates/condukt/src/state.rs) writes
//! `<run_id>.json` with the RAW run id, while `run_path` / `run_dir` — used by
//! `RunState::load` and every reader — address `<safe_session(run_id)>.json`.
//! For a run id that `safe_session` rewrites (any char outside
//! `[A-Za-z0-9_.-]`, e.g. a space), `state init --run <id>` succeeds and
//! prints the id, but the run it just created cannot be read back by that same
//! id: save and load use different paths.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn condukt(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_condukt"))
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .stdin(Stdio::null())
        .output()
        .expect("condukt spawns")
}

fn s(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn repo(base: &Path) -> PathBuf {
    let repo = base.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let ok = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(&repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .unwrap()
        .success();
    assert!(ok, "git init");
    repo
}

#[test]
fn a_run_created_with_a_sanitised_id_is_readable_by_that_id() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = repo(tmp.path());
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let dec = tmp.path().join("d.json");
    std::fs::write(&dec, r#"{"goal":"g","tasks":[{"id":"t1","title":"x"}]}"#).unwrap();

    let run = "run with space";
    let init = condukt(
        &repo,
        &home,
        &[
            "state",
            "init",
            "--run",
            run,
            "--file",
            dec.to_str().unwrap(),
        ],
    );
    assert!(init.status.success(), "init: {}", s(&init.stderr));
    assert_eq!(s(&init.stdout).trim(), run, "init must print the run id");

    let shown = condukt(&repo, &home, &["state", "show", "--run", run]);
    assert!(
        shown.status.success(),
        "`state init --run {run:?}` succeeded, but `state show --run {run:?}` cannot \
         find the run it created (save wrote the raw id, load reads the sanitised one): \
         rc={:?} init.stderr={} show.stderr={}",
        shown.status.code(),
        s(&init.stderr),
        s(&shown.stderr)
    );
}
