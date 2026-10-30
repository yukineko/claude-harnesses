//! Integration tests for `compass discovery` driving the real binary, with an
//! isolated `HOME` (tempdir) so `harness_core::discovery::record_path` lands in a
//! throwaway `~/.compass` rather than the developer's real one.
//!
//! Coverage:
//! - DoD#2 cross-session dedup: a task discovered by another session is NOT
//!   re-surfaced by `gap` for this session (and shows in `discovery list`).
//! - DoD#3 selected transition: `discovery select` flips one row to `selected`
//!   while siblings stay `discovered`.
//! - DoD#4 byte-equivalent fallback: with no discovery store, `list --json` is
//!   `[]` and `gap` surfaces the opportunity unchanged (filter drops nothing).

use std::path::Path;
use std::process::Command;

use serde_json::Value;
use tempfile::TempDir;

/// A run environment: an isolated HOME (where the discovery store lands) and a
/// stable project cwd (which keys the per-project store).
struct Env {
    home: TempDir,
    proj: TempDir,
}

impl Env {
    fn new() -> Self {
        Env {
            home: tempfile::tempdir().expect("home tempdir"),
            proj: tempfile::tempdir().expect("proj tempdir"),
        }
    }

    /// Run `compass <args...>` with the given session id, isolated HOME and the
    /// fixed project cwd. Returns (stdout, success).
    fn run(&self, session: Option<&str>, args: &[&str]) -> (String, bool) {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_compass"));
        harness_core::git_env::scrub(&mut cmd);
        cmd.current_dir(self.proj.path())
            .env("HOME", self.home.path())
            .args(args);
        match session {
            Some(s) => {
                cmd.env("CLAUDE_CODE_SESSION_ID", s);
            }
            None => {
                cmd.env_remove("CLAUDE_CODE_SESSION_ID");
            }
        }
        let out = cmd.output().expect("spawn compass");
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        (stdout, out.status.success())
    }

    fn proj_path(&self) -> &Path {
        self.proj.path()
    }
}

/// Persist a minimal charter (north_star + DoD) via `compass charter --write` so
/// `gap` has something to load and surface opportunities under.
fn write_charter(env: &Env, north_star: &str) {
    let json = serde_json::json!({
        "north_star": north_star,
        "definition_of_done": ["surface the right next move"],
    })
    .to_string();
    let (_out, ok) = env.run(Some("setup"), &["charter", "--write", &json]);
    assert!(ok, "charter --write should succeed");
}

/// The `opportunities[].title` array from a `compass gap` JSON dump.
fn gap_opportunity_titles(env: &Env, session: &str) -> Vec<String> {
    let (out, ok) = env.run(Some(session), &["gap"]);
    assert!(ok, "gap should exit 0, got: {out}");
    let v: Value = serde_json::from_str(&out).expect("gap emits JSON");
    v["opportunities"]
        .as_array()
        .expect("opportunities is an array")
        .iter()
        .filter_map(|o| o["title"].as_str().map(str::to_string))
        .collect()
}

#[test]
fn dod4_absent_store_list_json_is_empty_array() {
    let env = Env::new();
    let (out, ok) = env.run(Some("sessA"), &["discovery", "list", "--json"]);
    assert!(ok, "discovery list must exit 0 even with no store");
    let v: Value = serde_json::from_str(out.trim()).expect("valid JSON");
    assert_eq!(v, serde_json::json!([]), "absent store => [], got {out}");
    // The store file must not have been created merely by listing.
    let harness_core::verdict::Determination::Known(store) =
        harness_core::discovery::record_path(env.proj_path())
    else {
        panic!("test project root must be determinate");
    };
    assert!(!store.exists(), "list must not create the store");
}

#[test]
fn dod2_other_session_discovery_is_not_resurfaced_by_gap() {
    let env = Env::new();
    write_charter(&env, "ship the dedup loop");

    // Ownership is by EARLIEST discovery, so the owner must record FIRST: sessA
    // discovers "dup task" before sessB ever sees it. sessA is now its owner.
    let (_r, ok) = env.run(
        Some("sessA"),
        &[
            "discovery",
            "record",
            "--session-id",
            "sessA",
            "--title",
            "dup task",
        ],
    );
    assert!(ok, "discovery record should exit 0");

    // sessB LATER registers the same "dup task" plus a unique "solo task" as
    // opportunities (each also emits a sessB discovery row, but sessB's "dup task"
    // row is later than sessA's, so sessA remains the owner).
    for title in ["dup task", "solo task"] {
        let (_o, ok) = env.run(Some("sessB"), &["opportunity", "add", "--title", title]);
        assert!(ok, "opportunity add should succeed");
    }

    // DoD#2: gap as the LATER session (sessB) drops "dup task" (owned by sessA's
    // earlier discovery) but keeps "solo task". The surviving "solo task" also
    // exercises DoD#4 byte-equivalence: an opportunity no other session owns is
    // never dropped by the filter.
    let after = gap_opportunity_titles(&env, "sessB");
    assert!(
        !after.contains(&"dup task".to_string()),
        "a task an EARLIER session discovered must NOT re-surface for the later session, got {after:?}"
    );
    assert!(
        after.contains(&"solo task".to_string()),
        "solo task (owned by nobody else) must still surface, got {after:?}"
    );

    // And the owner (sessA) still surfaces "dup task" — stable single owner, no
    // mutual annihilation where both sessions drop it.
    let owner_view = gap_opportunity_titles(&env, "sessA");
    assert!(
        owner_view.contains(&"dup task".to_string()),
        "the earliest discoverer (sessA) keeps surfacing its task, got {owner_view:?}"
    );

    // And the duplicate is observable in the shared store via `discovery list`.
    let (list, ok) = env.run(Some("sessB"), &["discovery", "list", "--json"]);
    assert!(ok);
    let rows: Value = serde_json::from_str(&list).expect("list JSON");
    let has_sessa_dup = rows.as_array().expect("array").iter().any(|r| {
        r["session_id"] == "sessA" && r["status"] == "discovered" && r["title"] == "dup task"
    });
    assert!(
        has_sessa_dup,
        "sessA's discovered dup row must be present: {list}"
    );
}

#[test]
fn dod3_select_flips_only_matching_row_to_selected() {
    let env = Env::new();

    // Two distinct rows from sessA.
    let (_a, _) = env.run(
        Some("sessA"),
        &[
            "discovery",
            "record",
            "--session-id",
            "sessA",
            "--title",
            "dup task",
        ],
    );
    let (_b, _) = env.run(
        Some("sessA"),
        &[
            "discovery",
            "record",
            "--session-id",
            "sessA",
            "--title",
            "other task",
        ],
    );

    // Select "dup task" by title.
    let (_s, ok) = env.run(
        Some("sessA"),
        &[
            "discovery",
            "select",
            "--session-id",
            "sessA",
            "--title",
            "dup task",
        ],
    );
    assert!(ok, "discovery select should exit 0");

    let (list, ok) = env.run(Some("sessA"), &["discovery", "list", "--json"]);
    assert!(ok);
    let rows: Value = serde_json::from_str(&list).expect("list JSON");
    let rows = rows.as_array().expect("array");

    let status_for = |title: &str| -> String {
        rows.iter()
            .find(|r| r["title"] == title)
            .and_then(|r| r["status"].as_str())
            .unwrap_or("missing")
            .to_string()
    };
    assert_eq!(status_for("dup task"), "selected", "selected title flips");
    assert_eq!(
        status_for("other task"),
        "discovered",
        "sibling row stays discovered"
    );
}

#[test]
fn select_without_args_is_a_noop_and_exits_zero() {
    let env = Env::new();
    // No --fingerprint, no --title: prints a note to stderr, still exits 0.
    let (_out, ok) = env.run(Some("sessA"), &["discovery", "select"]);
    assert!(ok, "select with no target must still exit 0");
}
