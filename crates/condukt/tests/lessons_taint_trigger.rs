//! End-to-end coverage for the 3rd taintguard trigger: "this turn consumed
//! cross-project lessons". `condukt lessons record-retrieval` is the
//! deterministic binary a SKILL Phase-1 step spawns to search the machine-
//! global lessons store and emit `lessons_context` for injection into the
//! transcript. When that search actually hits (injects untrusted-provenance
//! content — lessons other sessions/projects wrote), this turn's session must
//! be marked tainted via `taintguard::state::mark(&cwd, &run, "lessons")` so
//! taintguard's PreToolUse `gate` downgrades write-class tools for the rest
//! of the turn — mirroring the existing web/external-read triggers.
//!
//! State is isolated via `TAINTGUARD_STATE_DIR` (taintguard's marker base,
//! `crates/taintguard/src/state.rs`) and `LESSONS_STORE_DIR` (the
//! machine-global lessons store override, `crates/harness-core/src/
//! lessons.rs`), both honored only when absolute.

use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

struct Fixture {
    repo: PathBuf,
    taint_dir: PathBuf,
    lessons_dir: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let pid = std::process::id();
        let mut base = std::env::temp_dir();
        base.push(format!("condukt-lessons-taint-{pid}-{tag}"));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let taint_dir = base.join("taintguard-state");
        let lessons_dir = base.join("lessons-store");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&taint_dir).unwrap();
        std::fs::create_dir_all(&lessons_dir).unwrap();
        run_git(&repo, &["init", "-q", "-b", "main"]);
        run_git(&repo, &["config", "user.email", "t@t.t"]);
        run_git(&repo, &["config", "user.name", "t"]);
        Self {
            repo,
            taint_dir,
            lessons_dir,
        }
    }

    /// Seed the (isolated) global lessons store with one lesson whose
    /// `task_summary`/`lesson_text` contain `token` so a query for `token`
    /// deterministically hits it (lexical Jaccard search).
    fn seed_lesson(&self, id: &str, token: &str) {
        let path = self.lessons_dir.join("lessons.jsonl");
        let line = serde_json::json!({
            "id": id,
            "kind": "convention",
            "task_summary": format!("a task about {token} handling"),
            "lesson_text": format!("always remember the {token} convention"),
            "source_run": "run-seed",
            "ts": 1_700_000_000u64,
        });
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(f, "{}", line).unwrap();
    }

    fn record_retrieval(&self, run: &str, query: &str) -> std::process::Output {
        Command::new(bin())
            .args([
                "lessons",
                "record-retrieval",
                "--run",
                run,
                "--query",
                query,
            ])
            .current_dir(&self.repo)
            .env("TAINTGUARD_STATE_DIR", &self.taint_dir)
            .env("LESSONS_STORE_DIR", &self.lessons_dir)
            .output()
            .expect("spawn condukt lessons record-retrieval")
    }
}

fn run_git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git")
        .status
        .success();
    assert!(ok, "git {args:?} failed");
}

#[test]
fn nonempty_hit_taints_the_session_with_lessons_source() {
    let fx = Fixture::new("hit");
    fx.seed_lesson("l1", "frobnicate");

    let run = "sess-hit";
    let out = fx.record_retrieval(run, "frobnicate");
    assert!(
        out.status.success(),
        "record-retrieval should exit 0: {out:?}"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let arr: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON array");
    assert!(
        arr.as_array().map(|a| !a.is_empty()).unwrap_or(false),
        "expected a non-empty lessons_context for a matching query, got: {stdout}"
    );

    // Isolate taintguard's own `check` reads the same env override this test
    // set for the spawned process.
    std::env::set_var("TAINTGUARD_STATE_DIR", &fx.taint_dir);
    match taintguard::state::check(&fx.repo, run) {
        taintguard::state::Check::Tainted(sources) => {
            assert!(
                sources.iter().any(|s| s == "lessons"),
                "expected \"lessons\" among taint sources, got: {sources:?}"
            );
        }
        other => panic!("expected Tainted after a non-empty lessons retrieval, got {other:?}"),
    }
}

#[test]
fn zero_hit_does_not_taint_the_session() {
    // Empty/absent store: no lesson seeded at all.
    let fx = Fixture::new("miss");

    let run = "sess-miss";
    let out = fx.record_retrieval(run, "nothing-will-match-this-query-xyz");
    assert!(
        out.status.success(),
        "record-retrieval should exit 0: {out:?}"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let arr: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON array");
    assert!(
        arr.as_array().map(|a| a.is_empty()).unwrap_or(false),
        "expected an empty lessons_context for a zero-hit store, got: {stdout}"
    );

    std::env::set_var("TAINTGUARD_STATE_DIR", &fx.taint_dir);
    match taintguard::state::check(&fx.repo, run) {
        taintguard::state::Check::Clean => {}
        other => panic!(
            "a zero-hit retrieval must NOT taint the session (preserves the [] no-op contract), got {other:?}"
        ),
    }
}

#[test]
fn regression_retrieval_ledger_still_records_the_event() {
    // Existing behavior (retrieval ledger / hit flag) must remain unchanged
    // regardless of the new taint side-effect. Drive the ledger via `stats`
    // (same isolated LESSONS_STORE_DIR — the ledger is a separate machine-
    // global store under the harness config base, not env-overridden here,
    // so this test only asserts the command still succeeds and shape holds).
    let fx = Fixture::new("regress");
    fx.seed_lesson("l2", "widget");

    let out = fx.record_retrieval("sess-regress", "widget");
    assert!(out.status.success(), "record-retrieval failed: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let arr: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON array");
    let items = arr.as_array().expect("array");
    assert_eq!(
        items.len(),
        1,
        "expected exactly the one seeded lesson to hit"
    );
    assert_eq!(items[0]["id"], "l2");
    assert!(
        items[0].get("score").is_some(),
        "each hit lesson must carry a score field (fugu-router lessons search parity)"
    );
}
