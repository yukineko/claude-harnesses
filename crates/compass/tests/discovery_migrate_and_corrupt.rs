//! DoD14 move A, contract addendum items 6 and 7 (user rulings 2026-10-27):
//!
//! 6. A store file (canonical OR a worktree's legacy file) holding a non-empty
//!    line that is not a `DiscoveryRecord` is UNDETERMINED — it is not skipped.
//!    `list --json` refuses (non-zero, `undetermined`, no `[]`); `gap` keeps its
//!    candidate and says `discovery_dedup.status = undetermined`. Blank lines
//!    are not corrupt (control).
//! 7. `compass discovery migrate-legacy` copies every legacy worktree-keyed row
//!    (`$HOME/.compass/<store::project_key(<canonical worktree path>)>/discovery.jsonl`
//!    for each linked worktree) into the canonical store, without loss, without
//!    duplicates, leaving the legacy files byte-identical, idempotently, and
//!    all-or-nothing (an unreadable or corrupt source leaves the canonical store
//!    unchanged / absent).
//!
//! Black-box: every assertion drives the real `compass` binary with `HOME`
//! pointed at a tempdir, inside a throwaway git repo with linked worktrees made
//! by `git worktree add`. Legacy keys are computed by calling
//! `harness_core::store::project_key` directly (harness-core is a normal
//! dependency of compass, so integration tests can link it).
//!
//! The canonical store is located WITHOUT assuming its key: it is "every
//! `$HOME/.compass/*/discovery.jsonl` that is not one of the two legacy paths".
//!
//! At the RED commit the `migrate-legacy` subcommand does not exist (clap
//! rejects it with a non-zero exit), so every item-7 test fails there on its
//! exit-status / `undetermined` assertion; the messages say so.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

const CHARTER_JSON: &str = r#"{"north_star":"ship it","definition_of_done":["tests pass"],"measuring_stick":"tests passing","current_gap":"gap","next_action":"act","parked":[]}"#;
const OPP_TITLE: &str = "shared opportunity under test";
const CORRUPT_LINE: &str = "this line is not a discovery record\n";
const MISSING_AT_HEAD: &str =
    "(at the RED commit `compass discovery migrate-legacy` does not exist, so clap exits non-zero)";

struct Fixture {
    _tmp: tempfile::TempDir,
    base: PathBuf,
    r: PathBuf,
    w1: PathBuf,
    w2: PathBuf,
    home: PathBuf,
}

fn git(cwd: &Path, args: &[&str]) {
    let out = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "SETUP: git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run(home: &Path, cwd: &Path, session: &str, args: &[&str]) -> Out {
    let out = harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_compass")))
        .args(args)
        .env("HOME", home)
        .env("CLAUDE_CODE_SESSION_ID", session)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env_remove("HARNESS_SANDBOX_HOME")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .expect("compass runs");
    Out {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// R (primary, one commit holding a charter), W1 and W2 linked worktrees.
fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let r = base.join("repo");
    let home = base.join("home");
    std::fs::create_dir_all(&r).unwrap();
    std::fs::create_dir_all(&home).unwrap();

    let scratch = base.join("scratch");
    let scratch_home = base.join("scratch-home");
    std::fs::create_dir_all(&scratch).unwrap();
    std::fs::create_dir_all(&scratch_home).unwrap();
    let o = run(
        &scratch_home,
        &scratch,
        "setup",
        &["charter", "--write", CHARTER_JSON],
    );
    assert_eq!(o.code, 0, "SETUP: charter seed failed: {}", o.stderr);

    git(&r, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(r.join(".compass")).unwrap();
    std::fs::copy(
        scratch.join(".compass/charter.md"),
        r.join(".compass/charter.md"),
    )
    .unwrap();
    std::fs::write(
        r.join(".gitignore"),
        ".compass/opportunities.json\n.compass/outcomes.json\n.compass/carve-state.json\n.claude/\n",
    )
    .unwrap();
    git(&r, &["add", "-A"]);
    git(&r, &["commit", "-q", "-m", "init"]);
    let w1 = base.join("w1");
    let w2 = base.join("w2");
    git(
        &r,
        &["worktree", "add", "-q", "-b", "f1", w1.to_str().unwrap()],
    );
    git(
        &r,
        &["worktree", "add", "-q", "-b", "f2", w2.to_str().unwrap()],
    );
    Fixture {
        _tmp: tmp,
        base,
        r,
        w1,
        w2,
        home,
    }
}

fn seed_opportunity(fx: &Fixture, root: &Path) {
    let scratch = fx.base.join("opp-scratch");
    let scratch_home = fx.base.join("opp-scratch-home");
    std::fs::create_dir_all(&scratch).unwrap();
    std::fs::create_dir_all(&scratch_home).unwrap();
    let o = run(
        &scratch_home,
        &scratch,
        "setup",
        &["charter", "--write", CHARTER_JSON],
    );
    assert_eq!(o.code, 0, "SETUP: opp-scratch charter failed: {}", o.stderr);
    let o = run(
        &scratch_home,
        &scratch,
        "setup",
        &["opportunity", "add", "--title", OPP_TITLE],
    );
    assert_eq!(o.code, 0, "SETUP: opportunity add failed: {}", o.stderr);
    std::fs::create_dir_all(root.join(".compass")).unwrap();
    std::fs::copy(
        scratch.join(".compass/opportunities.json"),
        root.join(".compass/opportunities.json"),
    )
    .unwrap();
}

/// The OLD (pre-fix) worktree-keyed store path for linked worktree `wt`.
fn legacy_store(fx: &Fixture, wt: &Path) -> PathBuf {
    let top = std::fs::canonicalize(wt).unwrap();
    fx.home
        .join(".compass")
        .join(harness_core::store::project_key(&top))
        .join("discovery.jsonl")
}

/// Every `$HOME/.compass/*/discovery.jsonl` currently present (file OR dir).
fn store_files(home: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(home.join(".compass")) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path().join("discovery.jsonl"))
        .filter(|p| p.exists())
        .collect();
    out.sort();
    out
}

/// Store files that are NOT W1's / W2's legacy path (i.e. the canonical store).
fn canonical_files(fx: &Fixture) -> Vec<PathBuf> {
    let legacy = [legacy_store(fx, &fx.w1), legacy_store(fx, &fx.w2)];
    store_files(&fx.home)
        .into_iter()
        .filter(|p| !legacy.contains(p))
        .collect()
}

fn the_canonical_file(fx: &Fixture) -> PathBuf {
    let c = canonical_files(fx);
    assert_eq!(
        c.len(),
        1,
        "expected exactly one canonical discovery store besides the legacy files, got {c:?}"
    );
    c[0].clone()
}

fn row(title: &str, session: &str, created_at: u64) -> String {
    serde_json::json!({
        "fingerprint": harness_core::discovery::fingerprint(title),
        "session_id": session,
        "status": "discovered",
        "created_at": created_at,
        "title": title,
    })
    .to_string()
}

fn write_lines(path: &Path, lines: &[String]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut s = String::new();
    for l in lines {
        s.push_str(l);
        if !l.ends_with('\n') {
            s.push('\n');
        }
    }
    std::fs::write(path, s).unwrap();
}

fn append_raw(path: &Path, text: &str) {
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .expect("SETUP: open store for append");
    f.write_all(text.as_bytes()).unwrap();
}

fn make_unreadable(path: &Path) {
    if path.exists() {
        std::fs::remove_file(path).expect("SETUP: remove store file");
    }
    std::fs::create_dir_all(path).expect("SETUP: create dir in place of store");
    std::fs::write(path.join("blocker"), "x").unwrap();
}

fn record_ok(fx: &Fixture, cwd: &Path, session: &str, title: &str) {
    let o = run(
        &fx.home,
        cwd,
        session,
        &[
            "discovery",
            "record",
            "--title",
            title,
            "--session-id",
            session,
        ],
    );
    assert_eq!(
        o.code,
        0,
        "SETUP: discovery record from {} failed: {}",
        cwd.display(),
        o.stderr
    );
}

fn list_titles(fx: &Fixture, cwd: &Path) -> Vec<String> {
    let o = run(&fx.home, cwd, "lister", &["discovery", "list", "--json"]);
    assert_eq!(
        o.code,
        0,
        "discovery list --json from {} must exit 0, stderr: {}",
        cwd.display(),
        o.stderr
    );
    let v: Value = serde_json::from_str(o.stdout.trim()).unwrap_or_else(|e| {
        panic!(
            "list --json from {} not JSON ({e}): {}",
            cwd.display(),
            o.stdout
        )
    });
    v.as_array()
        .expect("list --json prints an array")
        .iter()
        .filter_map(|r| r["title"].as_str().map(str::to_string))
        .collect()
}

fn is_empty_json_array(s: &str) -> bool {
    matches!(serde_json::from_str::<Value>(s.trim()), Ok(Value::Array(a)) if a.is_empty())
}

fn assert_undetermined(o: &Out, what: &str) {
    assert_ne!(
        o.code, 0,
        "{what}: must exit NON-ZERO when undetermined; stdout: {} stderr: {}",
        o.stdout, o.stderr
    );
    assert!(
        !is_empty_json_array(&o.stdout),
        "{what}: must NOT print an empty JSON array when undetermined; stdout: {}",
        o.stdout
    );
    assert!(
        o.stderr.to_lowercase().contains("undetermined"),
        "{what}: stderr must say `undetermined`; stderr: {}",
        o.stderr
    );
}

/// Non-blank lines of a store file, parsed as JSON objects.
fn rows_of(path: &Path) -> Vec<Value> {
    let text =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            serde_json::from_str(l)
                .unwrap_or_else(|e| panic!("canonical row is not JSON ({e}): {l}"))
        })
        .collect()
}

type Identity = (String, String, u64, String);

fn identity(v: &Value) -> Identity {
    (
        v["fingerprint"].as_str().unwrap_or_default().to_string(),
        v["session_id"].as_str().unwrap_or_default().to_string(),
        v["created_at"].as_u64().unwrap_or(u64::MAX),
        v["title"].as_str().unwrap_or_default().to_string(),
    )
}

fn migrate(fx: &Fixture, cwd: &Path) -> Out {
    run(&fx.home, cwd, "migrator", &["discovery", "migrate-legacy"])
}

fn migrate_ok(fx: &Fixture, cwd: &Path) -> Value {
    let o = migrate(fx, cwd);
    assert_eq!(
        o.code,
        0,
        "`compass discovery migrate-legacy` from {} must exit 0 {MISSING_AT_HEAD}; stdout: {} stderr: {}",
        cwd.display(),
        o.stdout,
        o.stderr
    );
    serde_json::from_str(o.stdout.trim()).unwrap_or_else(|e| {
        panic!(
            "migrate-legacy stdout must be one JSON object ({e}): {}",
            o.stdout
        )
    })
}

fn assert_migrate_undetermined(o: &Out, what: &str) {
    assert_ne!(
        o.code, 0,
        "{what}: migrate-legacy must exit NON-ZERO; stdout: {} stderr: {}",
        o.stdout, o.stderr
    );
    assert!(
        o.stderr.to_lowercase().contains("undetermined"),
        "{what}: migrate-legacy stderr must say `undetermined` {MISSING_AT_HEAD}; stderr: {}",
        o.stderr
    );
}

fn count(titles: &[String], t: &str) -> usize {
    titles.iter().filter(|x| x.as_str() == t).count()
}

// ---------------------------------------------------------------- item 6

#[test]
fn corrupt_line_in_canonical_store_list_is_undetermined() {
    let fx = fixture();
    record_ok(&fx, &fx.r, "other", "T valid row");
    let files = store_files(&fx.home);
    assert_eq!(files.len(), 1, "SETUP: expected one store, got {files:?}");
    append_raw(&files[0], CORRUPT_LINE);
    let o = run(&fx.home, &fx.r, "s1", &["discovery", "list", "--json"]);
    assert_undetermined(
        &o,
        "list --json from R with a corrupt line in the canonical store",
    );
}

#[test]
fn corrupt_line_in_canonical_store_gap_keeps_candidate_and_dedup_undetermined() {
    let fx = fixture();
    seed_opportunity(&fx, &fx.r);
    record_ok(&fx, &fx.r, "other", "T valid row");
    let files = store_files(&fx.home);
    assert_eq!(files.len(), 1, "SETUP: expected one store, got {files:?}");
    append_raw(&files[0], CORRUPT_LINE);
    let o = run(&fx.home, &fx.r, "s1", &["gap"]);
    assert_eq!(
        o.code, 0,
        "gap must exit 0 (turn not broken); stderr: {}",
        o.stderr
    );
    let v: Value = serde_json::from_str(&o.stdout)
        .unwrap_or_else(|e| panic!("gap not JSON ({e}): {}", o.stdout));
    assert!(
        v["opportunities"]
            .as_array()
            .expect("gap.opportunities is an array")
            .iter()
            .any(|o| o["title"] == OPP_TITLE),
        "gap must keep the opportunity when the store has a corrupt line; got {}",
        v["opportunities"]
    );
    let dd = &v["discovery_dedup"];
    assert_eq!(
        dd["status"], "undetermined",
        "corrupt line in the canonical store => discovery_dedup.status must be \"undetermined\"; got {dd}"
    );
    assert!(
        dd["reason"].as_str().is_some_and(|s| !s.trim().is_empty()),
        "undetermined discovery_dedup must carry a non-empty string reason; got {dd}"
    );
}

#[test]
fn corrupt_line_in_worktree_legacy_file_list_from_that_worktree_is_undetermined() {
    let fx = fixture();
    let legacy = legacy_store(&fx, &fx.w1);
    write_lines(
        &legacy,
        &[
            row("T legacy ok", "legacy-w1", 1_700_000_001),
            CORRUPT_LINE.to_string(),
        ],
    );
    let o = run(&fx.home, &fx.w1, "s1", &["discovery", "list", "--json"]);
    assert_undetermined(
        &o,
        "list --json from W1 with a corrupt line in W1's legacy file",
    );
}

#[test]
fn control_blank_lines_in_canonical_store_are_not_corrupt() {
    let fx = fixture();
    record_ok(&fx, &fx.r, "other", "T valid row");
    let files = store_files(&fx.home);
    assert_eq!(files.len(), 1, "SETUP: expected one store, got {files:?}");
    append_raw(&files[0], "\n   \n\n");
    let titles = list_titles(&fx, &fx.r);
    assert_eq!(
        titles,
        vec!["T valid row".to_string()],
        "control: blank lines are not corrupt; the one valid row must be listed"
    );
}

// ---------------------------------------------------------------- item 7

const TA: &str = "T legacy A from W1";
const TB: &str = "T legacy B from W1";
const TC: &str = "T legacy C from W2";
const TX: &str = "T X recorded canonically from R";

struct Seeded {
    legacy_w1: PathBuf,
    legacy_w2: PathBuf,
    legacy_w1_bytes: Vec<u8>,
    legacy_w2_bytes: Vec<u8>,
}

/// Canonical store holds X (recorded from R via the real binary). W1's legacy
/// file holds A, B; W2's legacy file holds C plus X's exact line (so X is
/// "already present").
fn seed_migration(fx: &Fixture) -> Seeded {
    record_ok(fx, &fx.r, "main-session", TX);
    let canon = the_canonical_file(fx);
    let x_line = std::fs::read_to_string(&canon)
        .unwrap()
        .lines()
        .find(|l| !l.trim().is_empty())
        .expect("SETUP: canonical row for X")
        .to_string();
    let legacy_w1 = legacy_store(fx, &fx.w1);
    let legacy_w2 = legacy_store(fx, &fx.w2);
    write_lines(
        &legacy_w1,
        &[
            row(TA, "legacy-w1", 1_700_000_001),
            row(TB, "legacy-w1", 1_700_000_002),
        ],
    );
    write_lines(&legacy_w2, &[row(TC, "legacy-w2", 1_700_000_003), x_line]);
    assert_eq!(
        rows_of(&canon).len(),
        1,
        "SETUP: canonical store must hold exactly X before migration"
    );
    Seeded {
        legacy_w1_bytes: std::fs::read(&legacy_w1).unwrap(),
        legacy_w2_bytes: std::fs::read(&legacy_w2).unwrap(),
        legacy_w1,
        legacy_w2,
    }
}

fn sources_map(summary: &Value) -> BTreeMap<String, u64> {
    summary["sources"]
        .as_array()
        .unwrap_or_else(|| panic!("summary.sources must be an array; got {summary}"))
        .iter()
        .map(|s| {
            (
                s["path"]
                    .as_str()
                    .unwrap_or_else(|| panic!("source.path must be a string; got {s}"))
                    .to_string(),
                s["rows"]
                    .as_u64()
                    .unwrap_or_else(|| panic!("source.rows must be a number; got {s}")),
            )
        })
        .collect()
}

#[test]
fn migrate_moves_legacy_rows_without_loss_or_dup_and_leaves_legacy_untouched() {
    let fx = fixture();
    let s = seed_migration(&fx);
    let summary = migrate_ok(&fx, &fx.r);

    assert_eq!(
        summary["migrated"], 3,
        "A, B (W1) and C (W2) are new => migrated must be 3; summary {summary}"
    );
    assert_eq!(
        summary["already_present"], 1,
        "X is in W2's legacy file AND the canonical store => already_present must be 1; summary {summary}"
    );
    let expected: BTreeMap<String, u64> = [
        (s.legacy_w1.to_string_lossy().into_owned(), 2u64),
        (s.legacy_w2.to_string_lossy().into_owned(), 2u64),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        sources_map(&summary),
        expected,
        "sources must list each linked worktree's legacy file with its row count (main excluded)"
    );

    let canon = the_canonical_file(&fx);
    let rows = rows_of(&canon);
    assert_eq!(
        rows.len(),
        4,
        "canonical store must hold X + A + B + C = 4 rows after migration; got {rows:?}"
    );
    let ids: BTreeSet<Identity> = rows.iter().map(identity).collect();
    assert_eq!(
        ids.len(),
        4,
        "no duplicated identity in canonical store: {rows:?}"
    );
    let titles: BTreeSet<String> = ids.iter().map(|i| i.3.clone()).collect();
    let want: BTreeSet<String> = [TA, TB, TC, TX].iter().map(|t| t.to_string()).collect();
    assert_eq!(titles, want, "canonical store must hold exactly X, A, B, C");

    assert_eq!(
        std::fs::read(&s.legacy_w1).unwrap(),
        s.legacy_w1_bytes,
        "W1's legacy file must be byte-for-byte untouched by migration"
    );
    assert_eq!(
        std::fs::read(&s.legacy_w2).unwrap(),
        s.legacy_w2_bytes,
        "W2's legacy file must be byte-for-byte untouched by migration"
    );
}

#[test]
fn migrate_is_idempotent_second_run_migrates_zero_and_canonical_is_byte_identical() {
    let fx = fixture();
    seed_migration(&fx);
    let first = migrate_ok(&fx, &fx.r);
    assert_eq!(
        first["migrated"], 3,
        "first run migrates 3; summary {first}"
    );
    let canon = the_canonical_file(&fx);
    let before = std::fs::read(&canon).unwrap();
    let second = migrate_ok(&fx, &fx.r);
    assert_eq!(
        second["migrated"], 0,
        "second run must migrate 0 rows; summary {second}"
    );
    assert_eq!(
        second["already_present"], 4,
        "second run: all 4 legacy rows are already present; summary {second}"
    );
    assert_eq!(
        std::fs::read(&canon).unwrap(),
        before,
        "second run must leave the canonical store byte-identical"
    );
}

#[test]
fn migrate_from_linked_worktree_targets_the_same_canonical_store() {
    let fx = fixture();
    seed_migration(&fx);
    let summary = migrate_ok(&fx, &fx.w1);
    assert_eq!(
        summary["migrated"], 3,
        "migrate-legacy from W1 must migrate the same 3 rows; summary {summary}"
    );
    let rows = rows_of(&the_canonical_file(&fx));
    assert_eq!(
        rows.len(),
        4,
        "migration from W1 must land in the one canonical store (X + A + B + C); got {rows:?}"
    );
}

#[test]
fn after_migration_each_row_is_listed_exactly_once_from_main_and_migrated_worktrees() {
    let fx = fixture();
    seed_migration(&fx);
    migrate_ok(&fx, &fx.r);
    for (label, cwd) in [("main R", &fx.r), ("W1", &fx.w1), ("W2", &fx.w2)] {
        let titles = list_titles(&fx, cwd);
        for t in [TA, TB, TC, TX] {
            assert_eq!(
                count(&titles, t),
                1,
                "after migration, `{t}` must be listed exactly once from {label}; got {titles:?}"
            );
        }
        assert_eq!(
            titles.len(),
            4,
            "after migration, {label} must list exactly 4 rows; got {titles:?}"
        );
    }
}

#[test]
fn migrate_with_unreadable_legacy_source_is_undetermined_and_writes_nothing() {
    let fx = fixture();
    let legacy_w1 = legacy_store(&fx, &fx.w1);
    let legacy_w2 = legacy_store(&fx, &fx.w2);
    write_lines(&legacy_w1, &[row(TA, "legacy-w1", 1_700_000_001)]);
    let w1_bytes = std::fs::read(&legacy_w1).unwrap();
    make_unreadable(&legacy_w2);
    assert!(
        canonical_files(&fx).is_empty(),
        "SETUP: canonical store must be absent before migration"
    );
    let o = migrate(&fx, &fx.r);
    assert_migrate_undetermined(&o, "a legacy source that is a directory");
    let c = canonical_files(&fx);
    assert!(
        c.is_empty(),
        "all-or-nothing: an unreadable source must leave the canonical store ABSENT; found {c:?}"
    );
    assert_eq!(
        std::fs::read(&legacy_w1).unwrap(),
        w1_bytes,
        "the readable legacy file must be untouched"
    );
}

#[test]
fn migrate_with_corrupt_legacy_line_is_undetermined_and_canonical_unchanged() {
    let fx = fixture();
    record_ok(&fx, &fx.r, "main-session", TX);
    let canon = the_canonical_file(&fx);
    let canon_before = std::fs::read(&canon).unwrap();
    write_lines(
        &legacy_store(&fx, &fx.w1),
        &[row(TA, "legacy-w1", 1_700_000_001)],
    );
    write_lines(
        &legacy_store(&fx, &fx.w2),
        &[
            row(TC, "legacy-w2", 1_700_000_003),
            CORRUPT_LINE.to_string(),
        ],
    );
    let o = migrate(&fx, &fx.r);
    assert_migrate_undetermined(&o, "a legacy source with a corrupt line");
    assert_eq!(
        canonical_files(&fx),
        vec![canon.clone()],
        "no new store may appear on a failed migration"
    );
    assert_eq!(
        std::fs::read(&canon).unwrap(),
        canon_before,
        "all-or-nothing: a corrupt source must leave the canonical store byte-identical"
    );
}

#[test]
fn migrate_with_undetermined_root_is_refused_and_writes_nothing() {
    let fx = fixture();
    write_lines(
        &legacy_store(&fx, &fx.w1),
        &[row(TA, "legacy-w1", 1_700_000_001)],
    );
    std::fs::write(fx.w2.join(".git"), "garbage\n").unwrap();
    let o = migrate(&fx, &fx.w2);
    assert_migrate_undetermined(
        &o,
        "migrate-legacy from a worktree with a corrupt .git gitfile",
    );
    let c = canonical_files(&fx);
    assert!(
        c.is_empty(),
        "undetermined root: migrate-legacy must write nothing; found {c:?}"
    );
}
