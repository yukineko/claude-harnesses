// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Closure regression tests written by the independent closure verifier
//! (audit batch b1_1, 2026-10-02). Each test names the backlog id(s) whose
//! closure it proves (or, for an ignored test, the still-open failure it pins).
//! Black-box: every test spawns the real `condukt` binary under an isolated HOME.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

struct Fx {
    base: PathBuf,
    repo: PathBuf,
    home: PathBuf,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git")
        .status
        .success();
    assert!(ok, "git {args:?} failed");
}

impl Fx {
    fn new(tag: &str) -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir().join(format!(
            "condukt-audit-v11-{tag}-{}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@t.t"]);
        git(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("base.txt"), "base\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        Self { base, repo, home }
    }

    fn condukt(&self, args: &[&str]) -> Output {
        Command::new(bin())
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .stdin(Stdio::null())
            .output()
            .expect("spawn condukt")
    }
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

// ---------------------------------------------------------------------------
// backlog 9eeffa69: flow Step 3-4 runs `condukt circuit check --run flow-<S>`;
// no condukt run state ever exists under that id, so the breaker tripped
// `idle_unmeasured` ("run state could not be loaded") after every cycle.
// Closure property: the invocation the flow SKILL documents (`--run flow-<S>
// --session <S>`), with no run state and a FRESH session-S transcript,
// continues (exit 0) instead of tripping idle_unmeasured.
// ---------------------------------------------------------------------------

const SESSION: &str = "9eeffa69-aaaa-bbbb-cccc-000000000001";

#[test]
fn backlog_9eeffa69_flow_circuit_check_with_session_and_no_run_state_continues() {
    let fx = Fx::new("9eeffa69");
    let tdir = fx.home.join(".claude").join("projects").join("-fixture");
    std::fs::create_dir_all(&tdir).unwrap();
    std::fs::write(tdir.join(format!("{SESSION}.jsonl")), "{\"seed\":1}\n").unwrap();

    let run = format!("flow-{SESSION}");
    let out = fx.condukt(&[
        "circuit",
        "check",
        "--run",
        &run,
        "--session",
        SESSION,
        "--idle-ttl-secs",
        "1800",
    ]);
    let stdout = text(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!(
            "circuit check stdout not JSON ({e}): {stdout} / {}",
            text(&out.stderr)
        )
    });
    assert_ne!(
        v["reason"], "idle_unmeasured",
        "flow's documented circuit check must not trip idle_unmeasured when no run state \
         exists but the session transcript is fresh: {v}"
    );
    assert_eq!(v["verdict"], "continue", "{v}");
    assert_eq!(out.status.code(), Some(0), "{v}");
}

/// The other half of 9eeffa69: the flow SKILL must actually pass `--session`
/// on every `condukt circuit check --run "flow-..."` invocation; without it the
/// binary fix is unreachable from /flow.
#[test]
fn backlog_9eeffa69_flow_skill_passes_session_to_circuit_check() {
    let skill = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("flow")
        .join("skills")
        .join("flow")
        .join("SKILL.md");
    let body = std::fs::read_to_string(&skill).expect("read flow SKILL.md");
    let calls: Vec<&str> = body
        .lines()
        .filter(|l| l.contains("condukt circuit check --run \"flow-"))
        .collect();
    assert!(
        !calls.is_empty(),
        "no flow-run circuit check found in {}",
        skill.display()
    );
    for l in calls {
        assert!(
            l.contains("--session \"$CLAUDE_CODE_SESSION_ID\""),
            "flow SKILL calls circuit check without --session: {l}"
        );
    }
}

// ---------------------------------------------------------------------------
// backlog 62000a3a: with the run's decomposition file absent, `state set
// --status verified` exited non-zero and two tests asserting exit 0 were red.
// Resolved by ruling 9a4fb884 (306cf008): an absent decomposition must still
// allow verification (durable status = verified, F->P gate does not refuse),
// and the terminal claim release — Undetermined because the task's files
// cannot be known — exits non-zero with the named diagnostic.
// ---------------------------------------------------------------------------

#[test]
fn backlog_62000a3a_absent_decomposition_verifies_and_surfaces_undetermined_release() {
    let fx = Fx::new("62000a3a");
    let dec = fx.repo.join("decomposition.json");
    std::fs::write(
        &dec,
        r#"{"goal":"g","tasks":[{"id":"task1","title":"t","kind":"fix","reproduction_tests":"cargo test -p demo"}]}"#,
    )
    .unwrap();
    let init = fx.condukt(&[
        "state",
        "init",
        "--run",
        "run-62000a3a",
        "--file",
        dec.to_str().unwrap(),
    ]);
    assert_eq!(init.status.code(), Some(0), "init: {}", text(&init.stderr));

    // Delete the PERSISTED decomposition (the copy condukt reads), not the input.
    fn walk(d: &Path, name: &str, hits: &mut Vec<PathBuf>) {
        if let Ok(rd) = std::fs::read_dir(d) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, name, hits);
                } else if p.file_name().and_then(|s| s.to_str()) == Some(name) {
                    hits.push(p);
                }
            }
        }
    }
    let mut hits = Vec::new();
    walk(&fx.home, "run-62000a3a.decomposition.json", &mut hits);
    assert_eq!(hits.len(), 1, "persisted decomposition: {hits:?}");
    std::fs::remove_file(&hits[0]).unwrap();

    let set = fx.condukt(&[
        "state",
        "set",
        "--run",
        "run-62000a3a",
        "--task",
        "task1",
        "--status",
        "verified",
    ]);
    let stderr = text(&set.stderr);
    assert!(
        !stderr.contains("refusing to verify"),
        "absent decomposition must not be refused by the F->P gate: {stderr}"
    );

    let show = fx.condukt(&["state", "show", "--run", "run-62000a3a"]);
    let v: serde_json::Value = serde_json::from_str(&text(&show.stdout)).expect("show JSON");
    let status = v["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "task1")
        .map(|t| t["status"].clone());
    assert_eq!(
        status,
        Some(serde_json::Value::from("verified")),
        "task must be durably verified; stderr: {stderr}"
    );

    assert_ne!(
        set.status.code(),
        Some(0),
        "Undetermined claim release must not exit 0 (ruling 9a4fb884): {stderr}"
    );
    assert!(
        stderr.contains("claim release NOT PERFORMED"),
        "non-zero exit must carry the named diagnostic: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// backlog 65d0c726 (DUPLICATE of 55f55932): `condukt state claim-task` with an
// EMPTY --hashkey exits 0 and records a task claim under the "" key. Both ids
// describe this one failure (no input validation in claim_tasks). Pinned RED
// until 55f55932 lands.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "backlog 55f55932 / 65d0c726 OPEN: empty hashkey is accepted and recorded"]
fn backlog_55f55932_65d0c726_empty_hashkey_claim_is_refused() {
    let fx = Fx::new("55f55932");
    let out = fx.condukt(&[
        "state",
        "claim-task",
        "--run",
        "flow-x",
        "--session",
        "sess-x",
        "--title",
        "d17107ad 1d8df0e8d35d6398",
        "--hashkey",
        "",
    ]);
    let claims = fx.condukt(&["state", "claims"]);
    let v: serde_json::Value =
        serde_json::from_str(&text(&claims.stdout)).unwrap_or(serde_json::Value::Null);
    let has_empty_key = v["task_claims"]
        .as_object()
        .map(|m| m.contains_key(""))
        .unwrap_or(false);
    assert!(
        out.status.code() != Some(0) && !has_empty_key,
        "empty hashkey must be refused non-zero and must not be recorded; exit={:?} \
         empty-key-recorded={has_empty_key} stdout={} claims={}",
        out.status.code(),
        text(&out.stdout),
        text(&claims.stdout)
    );
}
