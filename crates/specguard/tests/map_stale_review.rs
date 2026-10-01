#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! INDEPENDENT RED tests (task b1-specguard-stale-review-tests; implementation is
//! b2-specguard-stale-review-impl, written by a different agent).
//!
//! Contract (user ruling 2026-10-02):
//!   * `specguard map review-status [--json] [--max-commits N]` classifies every
//!     `tracked` entry as `fresh` / `stale-review` / `undetermined`;
//!   * `specguard map list --json` carries the same value per entry as
//!     `review_state`;
//!   * an entry is `stale-review` when it has no recorded review (`reviewed_at`
//!     absent — a legacy entry) or its `reviewed_at.commit` is MORE than N
//!     commits behind HEAD;
//!   * N defaults to 50 from `[map] review_max_commits` in `specguard.toml`; a
//!     missing key or `0` falls back to 50 — never "never stale";
//!   * `review-status` exits 0 when every tracked entry is fresh, 1 when any is
//!     stale-review, 2 when any is undetermined (same as `map gate-check`);
//!   * "cannot tell" is never `fresh` (CLAUDE.md §3). Orchestrator decision
//!     within that contract (2026-10-02, after b1 verification): when HEAD
//!     cannot be read at all (no git repo, or an unborn HEAD with no commits)
//!     the state is exactly `undetermined` and `review-status` exits 2. A
//!     `reviewed_at.commit` absent from history may be `stale-review` (exit 1)
//!     or `undetermined` (exit 2), never `fresh`;
//!   * the boundary is strict: exactly N commits behind is `fresh`, N+1 is
//!     `stale-review`.
//!
//! The `review-status --json` document shape is not fixed by the contract, so
//! [`state_in`] accepts the two natural shapes (an object keyed by entry key, or
//! an array of objects carrying `key`) under `entries`, or the same at top
//! level. Every test still pins the exact state VALUE for named keys, and the
//! anti-vacuity tests require `fresh` to be reported, so the tolerance cannot
//! make an assertion pass on missing output.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn init_repo(repo: &Path) {
    git(repo, &["init", "-q"]);
    git(repo, &["config", "user.email", "t@t.t"]);
    git(repo, &["config", "user.name", "t"]);
    git(repo, &["config", "commit.gpgsign", "false"]);
}

fn empty_commits(repo: &Path, n: usize) {
    for i in 0..n {
        git(
            repo,
            &["commit", "-q", "--allow-empty", "-m", &format!("c{i}")],
        );
    }
}

/// `specguard.toml`; `map_section` is the body of a `[map]` table (None = no
/// `[map]` table at all).
fn write_config(repo: &Path, map_section: Option<&str>) {
    let mut cfg = String::from(
        "[project]\nname = \"Demo\"\nroot = \".\"\n\n[output]\nreport_dir = \"reports\"\nsentinel = \".pending\"\n\n[[area]]\nname = \"src\"\nglobs = [\"src/**\"]\ncanon = [\"docs/spec.md\"]\n",
    );
    if let Some(body) = map_section {
        cfg.push_str("\n[map]\n");
        cfg.push_str(body);
        cfg.push('\n');
    }
    fs::write(repo.join("specguard.toml"), cfg).unwrap();
}

/// One store entry. `reviewed` = Some(commit) adds reviewed_reason/reviewed_at;
/// None writes a legacy entry with neither field present.
fn entry(key: &str, status: &str, reviewed: Option<&str>) -> String {
    let mut s = format!(
        "[entries.\"{key}\"]\nkey = \"{key}\"\nkind = \"feature\"\nstatus = \"{status}\"\nspec_doc = \"docs/spec.md\"\nimpl_files = [\"{key}\"]\n"
    );
    if let Some(c) = reviewed {
        s.push_str("reviewed_reason = \"looked\"\n");
        s.push_str(&format!(
            "[entries.\"{key}\".reviewed_at]\ncommit = \"{c}\"\ndate = \"2026-01-01\"\n"
        ));
    }
    s
}

fn write_store(repo: &Path, entries: &[String]) {
    let store = repo.join(".specguard/spec-map.toml");
    fs::create_dir_all(store.parent().unwrap()).unwrap();
    let mut s = String::from("last_synced = \"deadbeef\"\n");
    for e in entries {
        s.push_str(e);
    }
    fs::write(&store, s).unwrap();
}

fn sg(repo: &Path, args: &[&str]) -> Output {
    sg_cmd(repo, args).output().unwrap()
}

fn sg_cmd(repo: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_specguard"));
    // A test run inside a git hook inherits GIT_DIR / GIT_WORK_TREE, which
    // would point every git call at the outer repo.
    c.env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .current_dir(repo)
        .args(["--config", "specguard.toml", "--date", "2026-01-01", "map"])
        .args(args);
    c
}

/// Like [`sg`], but git discovery cannot climb above `repo`: the tempdir's
/// parent (both as given and canonicalized — macOS /var vs /private/var) is a
/// ceiling, so an ancestor repository can never make HEAD readable.
fn sg_isolated(repo: &Path, args: &[&str]) -> Output {
    let parent = repo.parent().unwrap();
    let canon = fs::canonicalize(parent).unwrap();
    let ceiling = std::env::join_paths([parent.to_path_buf(), canon]).unwrap();
    sg_cmd(repo, args)
        .env("GIT_CEILING_DIRECTORIES", ceiling)
        .output()
        .unwrap()
}

fn dump(out: &Output) -> String {
    format!(
        "exit {:?}\nstdout: {}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn json(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("stdout is not JSON ({e})\n{}", dump(out)))
}

/// Find the review state reported for `key` in a `review-status --json` doc.
fn state_in(doc: &serde_json::Value, key: &str) -> Option<String> {
    fn from(v: &serde_json::Value, key: &str) -> Option<String> {
        if let Some(obj) = v.as_object() {
            if let Some(e) = obj.get(key) {
                if let Some(s) = e.as_str() {
                    return Some(s.to_string());
                }
                if let Some(s) = e.get("review_state").and_then(|s| s.as_str()) {
                    return Some(s.to_string());
                }
            }
        }
        if let Some(arr) = v.as_array() {
            for e in arr {
                if e.get("key").and_then(|k| k.as_str()) == Some(key) {
                    return e
                        .get("review_state")
                        .or_else(|| e.get("state"))
                        .and_then(|s| s.as_str())
                        .map(str::to_string);
                }
            }
        }
        None
    }
    doc.get("entries")
        .and_then(|e| from(e, key))
        .or_else(|| from(doc, key))
}

/// `review_state` of `key` in `map list --json`.
fn list_state(repo: &Path, key: &str) -> Option<String> {
    let out = sg(repo, &["list", "--json"]);
    assert!(out.status.success(), "map list --json: {}", dump(&out));
    let j = json(&out);
    j["entries"][key]["review_state"]
        .as_str()
        .map(str::to_string)
}

fn review_status(repo: &Path, extra: &[&str]) -> (Output, serde_json::Value) {
    let mut args = vec!["review-status", "--json"];
    args.extend_from_slice(extra);
    let out = sg(repo, &args);
    let j = json(&out);
    (out, j)
}

/// Repo: seed commit R0 (review point), then `behind` empty commits. Returns
/// (repo dir, R0 sha, HEAD sha).
fn repo_with_history(
    behind: usize,
    map_section: Option<&str>,
) -> (tempfile::TempDir, String, String) {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    init_repo(repo);
    fs::write(repo.join("README.md"), "seed\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "seed"]);
    let r0 = git(repo, &["rev-parse", "HEAD"]);
    empty_commits(repo, behind);
    let head = git(repo, &["rev-parse", "HEAD"]);
    write_config(repo, map_section);
    (dir, r0, head)
}

fn assert_state(repo: &Path, extra: &[&str], key: &str, want: &str) {
    let (out, j) = review_status(repo, extra);
    assert_eq!(
        state_in(&j, key).as_deref(),
        Some(want),
        "review-status {extra:?}: {key} must be {want}\n{}",
        dump(&out)
    );
}

// ---------------------------------------------------------------- case 1

#[test]
fn legacy_tracked_entry_without_reviewed_at_is_stale_review() {
    let (d, _r0, head) = repo_with_history(0, None);
    write_store(
        d.path(),
        &[
            entry("src/legacy.rs", "tracked", None),
            entry("src/new.rs", "tracked", Some(&head)),
        ],
    );
    let (out, j) = review_status(d.path(), &[]);
    assert_eq!(
        state_in(&j, "src/legacy.rs").as_deref(),
        Some("stale-review"),
        "no recorded review is not a fresh review\n{}",
        dump(&out)
    );
    assert_eq!(
        state_in(&j, "src/new.rs").as_deref(),
        Some("fresh"),
        "{}",
        dump(&out)
    );
    assert_eq!(
        out.status.code(),
        Some(1),
        "any stale-review => exit 1\n{}",
        dump(&out)
    );
    assert_eq!(
        list_state(d.path(), "src/legacy.rs").as_deref(),
        Some("stale-review")
    );
    assert_eq!(list_state(d.path(), "src/new.rs").as_deref(), Some("fresh"));
}

// ---------------------------------------------------------------- case 2

#[test]
fn review_more_than_max_commits_behind_head_is_stale_review() {
    let (d, r0, _head) = repo_with_history(3, None);
    write_store(d.path(), &[entry("src/old.rs", "tracked", Some(&r0))]);
    let (out, j) = review_status(d.path(), &["--max-commits", "2"]);
    assert_eq!(
        state_in(&j, "src/old.rs").as_deref(),
        Some("stale-review"),
        "3 commits behind with N=2\n{}",
        dump(&out)
    );
    assert_eq!(out.status.code(), Some(1), "{}", dump(&out));
}

// ---------------------------------------------------------------- case 3

#[test]
fn review_within_max_commits_is_fresh_and_exit_zero() {
    let (d, r0, head) = repo_with_history(1, None);
    write_store(
        d.path(),
        &[
            entry("src/old.rs", "tracked", Some(&r0)),
            entry("src/new.rs", "tracked", Some(&head)),
        ],
    );
    let (out, j) = review_status(d.path(), &["--max-commits", "2"]);
    assert_eq!(
        state_in(&j, "src/old.rs").as_deref(),
        Some("fresh"),
        "{}",
        dump(&out)
    );
    assert_eq!(
        state_in(&j, "src/new.rs").as_deref(),
        Some("fresh"),
        "{}",
        dump(&out)
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "all tracked fresh => exit 0\n{}",
        dump(&out)
    );
    assert_eq!(list_state(d.path(), "src/old.rs").as_deref(), Some("fresh"));
}

#[test]
fn review_status_without_json_uses_same_exit_codes() {
    let (d, r0, _head) = repo_with_history(3, None);
    write_store(d.path(), &[entry("src/old.rs", "tracked", Some(&r0))]);
    let fresh = sg(d.path(), &["review-status", "--max-commits", "5"]);
    assert_eq!(fresh.status.code(), Some(0), "{}", dump(&fresh));
    let stale = sg(d.path(), &["review-status", "--max-commits", "2"]);
    assert_eq!(stale.status.code(), Some(1), "{}", dump(&stale));
}

// ---------------------------------------------------------------- case 4

/// Exit code must agree with the reported classification (contract exit
/// convention), and neither may be the all-fresh answer.
fn assert_never_fresh(out: &Output, j: &serde_json::Value, key: &str) {
    let st = state_in(j, key);
    assert!(
        matches!(st.as_deref(), Some("stale-review") | Some("undetermined")),
        "{key}: cannot-tell must be stale-review or undetermined, got {st:?}\n{}",
        dump(out)
    );
    let want = if st.as_deref() == Some("undetermined") {
        2
    } else {
        1
    };
    assert_eq!(out.status.code(), Some(want), "{}", dump(out));
}

/// HEAD unreadable ⇒ exactly `undetermined` and exit 2 (orchestrator
/// decision, see the module doc).
fn assert_undetermined_exit2(out: &Output, key: &str) {
    assert_eq!(
        out.status.code(),
        Some(2),
        "unreadable HEAD must exit 2 (undetermined)\n{}",
        dump(out)
    );
    let j = json(out);
    assert_eq!(
        state_in(&j, key).as_deref(),
        Some("undetermined"),
        "unreadable HEAD: {key} must be exactly undetermined\n{}",
        dump(out)
    );
}

fn fake_review_store(repo: &Path) {
    write_store(
        repo,
        &[entry(
            "src/old.rs",
            "tracked",
            Some("0123456789abcdef0123456789abcdef01234567"),
        )],
    );
}

#[test]
fn unreadable_head_is_never_fresh() {
    // A git dir with no commits: HEAD is unborn, so "how far behind HEAD" is
    // unanswerable.
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    init_repo(repo);
    write_config(repo, None);
    fake_review_store(repo);
    let out = sg_isolated(repo, &["review-status", "--json", "--max-commits", "50"]);
    assert_undetermined_exit2(&out, "src/old.rs");
}

#[test]
fn unreadable_head_without_git_at_all_is_never_fresh() {
    // No git repo at all, and discovery is fenced at the tempdir so an
    // ancestor repo cannot supply a HEAD.
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    write_config(repo, None);
    fake_review_store(repo);
    let out = sg_isolated(repo, &["review-status", "--json"]);
    assert_undetermined_exit2(&out, "src/old.rs");
}

// ------------------------------------------------------ exact boundary

#[test]
fn exactly_n_behind_is_fresh_and_n_plus_one_is_stale_review() {
    let (d, r0, _head) = repo_with_history(3, None);
    let n_behind = git(d.path(), &["rev-parse", "HEAD~2"]);
    write_store(
        d.path(),
        &[
            entry("src/at_n.rs", "tracked", Some(&n_behind)),
            entry("src/at_n_plus_1.rs", "tracked", Some(&r0)),
        ],
    );
    let (out, j) = review_status(d.path(), &["--max-commits", "2"]);
    assert_eq!(
        state_in(&j, "src/at_n.rs").as_deref(),
        Some("fresh"),
        "exactly N=2 behind is not MORE than N\n{}",
        dump(&out)
    );
    assert_eq!(
        state_in(&j, "src/at_n_plus_1.rs").as_deref(),
        Some("stale-review"),
        "N+1=3 behind\n{}",
        dump(&out)
    );
    assert_eq!(out.status.code(), Some(1), "{}", dump(&out));
}

#[test]
fn exactly_n_behind_alone_exits_zero_via_config_key() {
    let (d, _r0, _head) = repo_with_history(3, Some("review_max_commits = 2"));
    let n_behind = git(d.path(), &["rev-parse", "HEAD~2"]);
    write_store(
        d.path(),
        &[entry("src/at_n.rs", "tracked", Some(&n_behind))],
    );
    let (out, j) = review_status(d.path(), &[]);
    assert_eq!(
        state_in(&j, "src/at_n.rs").as_deref(),
        Some("fresh"),
        "{}",
        dump(&out)
    );
    assert_eq!(out.status.code(), Some(0), "{}", dump(&out));
    assert_eq!(
        list_state(d.path(), "src/at_n.rs").as_deref(),
        Some("fresh")
    );
}

// ------------------------------------------- list --json surface on its own

#[test]
fn list_json_carries_review_state_per_entry() {
    // No `[map]` key, so this observes the list surface alone (default N=50).
    let (d, r0, head) = repo_with_history(DEEP, None);
    write_store(
        d.path(),
        &[
            entry("src/legacy.rs", "tracked", None),
            entry("src/old.rs", "tracked", Some(&r0)),
            entry("src/new.rs", "tracked", Some(&head)),
        ],
    );
    assert_eq!(list_state(d.path(), "src/new.rs").as_deref(), Some("fresh"));
    assert_eq!(
        list_state(d.path(), "src/old.rs").as_deref(),
        Some("stale-review")
    );
    assert_eq!(
        list_state(d.path(), "src/legacy.rs").as_deref(),
        Some("stale-review")
    );
}

#[test]
fn review_commit_not_in_history_is_never_fresh() {
    let (d, _r0, head) = repo_with_history(1, None);
    write_store(
        d.path(),
        &[
            entry(
                "src/ghost.rs",
                "tracked",
                Some("0123456789abcdef0123456789abcdef01234567"),
            ),
            entry("src/new.rs", "tracked", Some(&head)),
        ],
    );
    let (out, j) = review_status(d.path(), &["--max-commits", "50"]);
    assert_eq!(
        state_in(&j, "src/new.rs").as_deref(),
        Some("fresh"),
        "{}",
        dump(&out)
    );
    assert_never_fresh(&out, &j, "src/ghost.rs");
    let ls = list_state(d.path(), "src/ghost.rs");
    assert!(
        matches!(ls.as_deref(), Some("stale-review") | Some("undetermined")),
        "map list review_state for unknown commit: {ls:?}"
    );
}

// ---------------------------------------------------------------- case 5

#[test]
fn non_tracked_entries_are_not_classified_stale_review() {
    let (d, r0, head) = repo_with_history(5, None);
    write_store(
        d.path(),
        &[
            entry("src/changed.rs", "changed", Some(&r0)),
            entry("src/missing.rs", "missing", None),
            entry("src/changed_legacy.rs", "changed", None),
            entry("src/new.rs", "tracked", Some(&head)),
        ],
    );
    let (out, j) = review_status(d.path(), &["--max-commits", "2"]);
    assert_eq!(
        state_in(&j, "src/new.rs").as_deref(),
        Some("fresh"),
        "{}",
        dump(&out)
    );
    for k in ["src/changed.rs", "src/missing.rs", "src/changed_legacy.rs"] {
        assert_ne!(
            state_in(&j, k).as_deref(),
            Some("stale-review"),
            "{k} is not tracked\n{}",
            dump(&out)
        );
        assert_ne!(
            list_state(d.path(), k).as_deref(),
            Some("stale-review"),
            "{k}"
        );
    }
    assert_eq!(
        out.status.code(),
        Some(0),
        "only non-tracked entries are old; every tracked entry is fresh\n{}",
        dump(&out)
    );
}

// ---------------------------------------------------------------- case 6

#[test]
fn legacy_store_without_review_fields_still_loads() {
    let (d, _r0, _head) = repo_with_history(0, None);
    write_store(
        d.path(),
        &[
            entry("src/legacy.rs", "tracked", None),
            entry("src/a.rs", "changed", None),
        ],
    );
    let out = sg(d.path(), &["list"]);
    assert!(out.status.success(), "{}", dump(&out));
    let out = sg(d.path(), &["list", "--json"]);
    assert!(out.status.success(), "{}", dump(&out));
    let j = json(&out);
    assert_eq!(
        j["entries"]["src/legacy.rs"]["status"].as_str(),
        Some("tracked"),
        "{j}"
    );
}

// ------------------------------------------------- config key default

const DEEP: usize = 55; // > 50, so default-50 says stale, "never stale" says fresh

#[test]
fn no_config_key_defaults_to_fifty() {
    let (d, r0, _head) = repo_with_history(DEEP, None);
    let near = git(d.path(), &["rev-parse", "HEAD~3"]);
    write_store(
        d.path(),
        &[
            entry("src/deep.rs", "tracked", Some(&r0)),
            entry("src/near.rs", "tracked", Some(&near)),
        ],
    );
    let (out, j) = review_status(d.path(), &[]);
    assert_eq!(
        state_in(&j, "src/near.rs").as_deref(),
        Some("fresh"),
        "{}",
        dump(&out)
    );
    assert_eq!(
        state_in(&j, "src/deep.rs").as_deref(),
        Some("stale-review"),
        "55 commits behind with default N=50\n{}",
        dump(&out)
    );
    assert_eq!(out.status.code(), Some(1), "{}", dump(&out));
}

#[test]
fn map_table_without_key_defaults_to_fifty() {
    let (d, r0, _head) = repo_with_history(DEEP, Some("path = \".specguard/spec-map.toml\""));
    let near = git(d.path(), &["rev-parse", "HEAD~3"]);
    write_store(
        d.path(),
        &[
            entry("src/deep.rs", "tracked", Some(&r0)),
            entry("src/near.rs", "tracked", Some(&near)),
        ],
    );
    assert_state(d.path(), &[], "src/near.rs", "fresh");
    assert_state(d.path(), &[], "src/deep.rs", "stale-review");
}

#[test]
fn config_key_zero_falls_back_to_fifty_not_never_stale() {
    let (d, r0, _head) = repo_with_history(DEEP, Some("review_max_commits = 0"));
    let near = git(d.path(), &["rev-parse", "HEAD~3"]);
    write_store(
        d.path(),
        &[
            entry("src/deep.rs", "tracked", Some(&r0)),
            entry("src/near.rs", "tracked", Some(&near)),
        ],
    );
    let (out, j) = review_status(d.path(), &[]);
    assert_eq!(
        state_in(&j, "src/near.rs").as_deref(),
        Some("fresh"),
        "{}",
        dump(&out)
    );
    assert_eq!(
        state_in(&j, "src/deep.rs").as_deref(),
        Some("stale-review"),
        "key=0 must fall back to 50, never mean never-stale\n{}",
        dump(&out)
    );
    assert_eq!(out.status.code(), Some(1), "{}", dump(&out));
    assert_eq!(
        list_state(d.path(), "src/deep.rs").as_deref(),
        Some("stale-review")
    );
    assert_eq!(
        list_state(d.path(), "src/near.rs").as_deref(),
        Some("fresh")
    );
}

#[test]
fn config_key_sets_n() {
    let (d, r0, _head) = repo_with_history(4, Some("review_max_commits = 2"));
    let near = git(d.path(), &["rev-parse", "HEAD~1"]);
    write_store(
        d.path(),
        &[
            entry("src/old.rs", "tracked", Some(&r0)),
            entry("src/near.rs", "tracked", Some(&near)),
        ],
    );
    let (out, j) = review_status(d.path(), &[]);
    assert_eq!(
        state_in(&j, "src/near.rs").as_deref(),
        Some("fresh"),
        "{}",
        dump(&out)
    );
    assert_eq!(
        state_in(&j, "src/old.rs").as_deref(),
        Some("stale-review"),
        "4 behind with review_max_commits=2\n{}",
        dump(&out)
    );
    assert_eq!(
        list_state(d.path(), "src/old.rs").as_deref(),
        Some("stale-review")
    );
}
