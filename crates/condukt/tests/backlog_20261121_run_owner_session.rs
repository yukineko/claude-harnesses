// このファイルは丸ごと integration test なので unwrap/expect を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! RED (independent test author): `condukt state init` must record the owning
//! Claude session as `owner_session`, and `state list-tasks` must expose it.
//!
//! Observed 2026-11-21: flow-watch (crates/flow/bin/flow-watch) emitted STATE
//! events in session 62e92d16 for `run-20261121-040142-44869`, a run of a
//! DIFFERENT session. The only run-level identity condukt recorded was
//! `terminal_label` (`pid-44869`: the pid of the short-lived `state init`
//! process, `main.rs::resolve_label`), which names no session. Main-session
//! ruling 2026-11-21: `state init` records `$CLAUDE_CODE_SESSION_ID` as
//! `owner_session`; when the env is unset the field is `null` (undetermined).
//! No value such as `pid-*` is fabricated.
//!
//! What these tests do NOT prove:
//!   * behaviour when `CLAUDE_CODE_SESSION_ID` is set but empty / whitespace
//!     (not ruled);
//!   * that `--label` and `owner_session` are independent beyond the default
//!     path (no `--label` is passed);
//!   * that other writers (`state set`, `cancel`, `reconcile`, ...) preserve
//!     the field on rewrite — only the init-time record is checked;
//!   * how runs created before the field existed are listed (old JSON without
//!     the key): only freshly-initialised runs are exercised.

use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

struct Fixture {
    base: PathBuf,
    repo: PathBuf,
    home: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let mut base = std::env::temp_dir();
        base.push(format!(
            "condukt-owner-session-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let home = base.join("home");
        std::fs::create_dir_all(home.join(".condukt").join("state")).unwrap();
        init_git_repo(&repo);
        Self { base, repo, home }
    }

    /// Run condukt with an isolated `$HOME`. `session`: `Some(id)` sets
    /// `CLAUDE_CODE_SESSION_ID`, `None` REMOVES it (the test process itself
    /// usually runs inside a Claude session and would otherwise leak its id).
    fn condukt(&self, args: &[&str], session: Option<&str>) -> std::process::Output {
        let mut cmd = Command::new(bin());
        harness_core::git_env::scrub(&mut cmd);
        cmd.args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env_remove("CONDUKT_STATE_DIR");
        match session {
            Some(s) => cmd.env("CLAUDE_CODE_SESSION_ID", s),
            None => cmd.env_remove("CLAUDE_CODE_SESSION_ID"),
        };
        cmd.output().expect("spawn condukt")
    }

    fn init_run(&self, session: Option<&str>) -> String {
        let decomp = self.repo.join("decomp.json");
        std::fs::write(
            &decomp,
            r#"{"goal":"g","tasks":[{"id":"t1","title":"x","touched_files":["a.rs"],"deps":[],"class":"serial","done_criteria":"d"}]}"#,
        )
        .unwrap();
        let out = self.condukt(
            &["state", "init", "--file", decomp.to_str().unwrap()],
            session,
        );
        assert!(out.status.success(), "state init failed: {out:?}");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .chain(String::from_utf8_lossy(&out.stderr).lines())
            .map(str::trim)
            .rev()
            .find(|l| l.starts_with("run-"))
            .expect("a run- id in init output")
            .to_string()
    }

    fn run_json(&self, rid: &str) -> serde_json::Value {
        let state = self.home.join(".condukt").join("state");
        let mut found = Vec::new();
        for e in std::fs::read_dir(&state).expect("state dir readable") {
            let p = e.expect("dir entry").path().join(format!("{rid}.json"));
            if p.exists() {
                found.push(p);
            }
        }
        assert_eq!(found.len(), 1, "exactly one run file for {rid}: {found:?}");
        serde_json::from_str(&std::fs::read_to_string(&found[0]).unwrap())
            .expect("run state is valid json")
    }

    /// The `state list-tasks` rows of run `rid` (asserts at least one).
    fn listed_rows(&self, rid: &str, session: Option<&str>) -> Vec<serde_json::Value> {
        let out = self.condukt(&["state", "list-tasks"], session);
        assert!(out.status.success(), "state list-tasks failed: {out:?}");
        let v: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("list-tasks stdout is JSON");
        let rows: Vec<_> = v
            .as_array()
            .expect("list-tasks is a JSON array")
            .iter()
            .filter(|r| r["run_id"] == rid)
            .cloned()
            .collect();
        assert!(
            !rows.is_empty(),
            "run {rid} has a pending task, so it must be listed: {v}"
        );
        rows
    }
}

fn init_git_repo(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["config", "user.email", "t@t.t"],
        &["config", "user.name", "t"],
    ] {
        git(dir, args);
    }
    std::fs::write(dir.join("base.txt"), "base\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", "init"]);
}

fn git(dir: &Path, args: &[&str]) {
    let out = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git");
    assert!(out.status.success(), "git {args:?} failed: {out:?}");
}

#[test]
fn state_init_records_owner_session_from_env() {
    let fx = Fixture::new("set");
    let rid = fx.init_run(Some("sess-owner-A"));
    let v = fx.run_json(&rid);
    assert_eq!(
        v.get("owner_session"),
        Some(&serde_json::json!("sess-owner-A")),
        "state init must record $CLAUDE_CODE_SESSION_ID as run-level `owner_session`; run json: {v}"
    );
}

#[test]
fn list_tasks_exposes_owner_session() {
    let fx = Fixture::new("list");
    let rid = fx.init_run(Some("sess-owner-B"));
    // list-tasks is read by a DIFFERENT session: the field must carry the
    // run's recorded owner, not the reader's env.
    for row in fx.listed_rows(&rid, Some("sess-reader-other")) {
        assert_eq!(
            row.get("owner_session"),
            Some(&serde_json::json!("sess-owner-B")),
            "every list-tasks row must carry the run's owner_session (not the reader's session): {row}"
        );
    }
}

#[test]
fn owner_session_is_null_when_env_unset() {
    let fx = Fixture::new("unset");
    let rid = fx.init_run(None);
    let v = fx.run_json(&rid);
    assert_eq!(
        v.get("owner_session"),
        Some(&serde_json::Value::Null),
        "with CLAUDE_CODE_SESSION_ID unset, owner_session must be present and null (undetermined) \
         — not missing, and never a fabricated value such as pid-*; run json: {v}"
    );
    for row in fx.listed_rows(&rid, None) {
        assert_eq!(
            row.get("owner_session"),
            Some(&serde_json::Value::Null),
            "list-tasks must expose the undetermined owner as an explicit null: {row}"
        );
    }
}
