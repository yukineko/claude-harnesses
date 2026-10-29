//! Legacy worktree-keyed discovery rows must not silently vanish when git is
//! unavailable.
//!
//! `harness_core::discovery::load` reads the canonical store (keyed on the main
//! worktree root, resolved purely from the filesystem) and then this checkout's
//! LEGACY worktree-keyed file. The legacy file's key comes from
//! `resolve_repo_root`, which asks `git rev-parse --show-toplevel` and, when git
//! cannot answer, falls back to `cwd.canonicalize()` — a guess. From a
//! subdirectory of a linked worktree that guess is the subdirectory, so the
//! legacy file is looked up under the wrong key, found absent, and its rows are
//! dropped while `list` exits 0 and `gap` claims dedup was `applied`.
//!
//! Required: when the legacy path cannot be resolved the load is Undetermined:
//! - `compass discovery list --json` exits non-zero, stdout is neither `[]` nor
//!   a partial list, stderr says `undetermined`;
//! - `compass gap` reports `discovery_dedup.status = "undetermined"`.
//!
//! Controls (same setup, git on PATH): the legacy row is listed and `gap`
//! dedups against it with status `applied` — so the RED cases fail only
//! because git is missing, not because the fixture is broken.
//!
//! git is made unavailable by running the compass binary (absolute path) with
//! `PATH=/nonexistent`. HOME is sandboxed to a tempdir.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

const CHARTER_JSON: &str = r#"{"north_star":"ship it","definition_of_done":["tests pass"],"measuring_stick":"tests passing","current_gap":"gap","next_action":"act","parked":[]}"#;
const LEGACY_TITLE: &str = "legacy row recorded by an older binary";

struct Fixture {
    _tmp: tempfile::TempDir,
    base: PathBuf,
    w1: PathBuf,
    /// A subdirectory of linked worktree W1 (the cwd under test).
    sub: PathBuf,
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

/// Run the compass binary. `git_on_path = false` sets `PATH=/nonexistent`
/// so no `git` can be spawned; the binary itself is exec'd by absolute path.
fn run(home: &Path, cwd: &Path, session: &str, args: &[&str], git_on_path: bool) -> Out {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_compass"));
    harness_core::git_env::scrub(&mut cmd);
    cmd.args(args)
        .env("HOME", home)
        .env("CLAUDE_CODE_SESSION_ID", session)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env_remove("HARNESS_SANDBOX_HOME")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if !git_on_path {
        cmd.env("PATH", "/nonexistent");
    }
    let out = cmd
        .output()
        .expect("compass binary is exec'd by absolute path");
    Out {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// Seed `<dir>/.compass/charter.md` (and, with `opp`, an opportunity titled
/// [`LEGACY_TITLE`]) via the real binary in a non-git scratch dir with its own
/// home, then copy into `dest`.
fn seed_compass_dir(base: &Path, dest: &Path, opp: bool) {
    let scratch = base.join(format!(
        "scratch-{}",
        dest.file_name().unwrap().to_string_lossy()
    ));
    let scratch_home = scratch.with_extension("home");
    std::fs::create_dir_all(&scratch).unwrap();
    std::fs::create_dir_all(&scratch_home).unwrap();
    let o = run(
        &scratch_home,
        &scratch,
        "setup",
        &["charter", "--write", CHARTER_JSON],
        true,
    );
    assert_eq!(o.code, 0, "SETUP: charter seed failed: {}", o.stderr);
    std::fs::create_dir_all(dest.join(".compass")).unwrap();
    std::fs::copy(
        scratch.join(".compass/charter.md"),
        dest.join(".compass/charter.md"),
    )
    .unwrap();
    if opp {
        let o = run(
            &scratch_home,
            &scratch,
            "setup",
            &["opportunity", "add", "--title", LEGACY_TITLE],
            true,
        );
        assert_eq!(o.code, 0, "SETUP: opportunity add failed: {}", o.stderr);
        std::fs::copy(
            scratch.join(".compass/opportunities.json"),
            dest.join(".compass/opportunities.json"),
        )
        .unwrap();
    }
}

/// Repo R (one commit), linked worktree W1, subdirectory W1/sub, and a legacy
/// worktree-keyed row for W1 recorded by another session.
fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let r = base.join("repo");
    let home = base.join("home");
    std::fs::create_dir_all(&r).unwrap();
    std::fs::create_dir_all(&home).unwrap();

    seed_compass_dir(&base, &r, false);
    std::fs::write(
        r.join(".gitignore"),
        ".compass/opportunities.json\n.compass/outcomes.json\n.compass/carve-state.json\n.claude/\nsub/\n",
    )
    .unwrap();
    git(&r, &["init", "-q", "-b", "main"]);
    git(&r, &["add", "-A"]);
    git(&r, &["commit", "-q", "-m", "init"]);
    let w1 = base.join("w1");
    git(
        &r,
        &["worktree", "add", "-q", "-b", "f1", w1.to_str().unwrap()],
    );
    let sub = w1.join("sub");
    std::fs::create_dir_all(&sub).unwrap();

    // The OLD worktree-keyed store for W1 (what an older binary wrote from W1).
    let legacy = home
        .join(".compass")
        .join(harness_core::store::project_key(
            &std::fs::canonicalize(&w1).unwrap(),
        ))
        .join("discovery.jsonl");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    let row = serde_json::json!({
        "fingerprint": harness_core::discovery::fingerprint(LEGACY_TITLE),
        "session_id": "legacy-session",
        "status": "discovered",
        "created_at": 1_700_000_000u64,
        "title": LEGACY_TITLE,
    });
    std::fs::write(&legacy, format!("{row}\n")).unwrap();

    Fixture {
        _tmp: tmp,
        base,
        w1,
        sub,
        home,
    }
}

fn parse_titles(o: &Out) -> Option<Vec<String>> {
    match serde_json::from_str::<Value>(o.stdout.trim()) {
        Ok(Value::Array(a)) => Some(
            a.iter()
                .filter_map(|r| r["title"].as_str().map(str::to_string))
                .collect(),
        ),
        _ => None,
    }
}

// ---------------------------------------------------------------- list

#[test]
fn control_with_git_lists_legacy_row_from_worktree_subdir() {
    let fx = fixture();
    let o = run(
        &fx.home,
        &fx.sub,
        "lister",
        &["discovery", "list", "--json"],
        true,
    );
    assert_eq!(
        o.code, 0,
        "control: list from W1/sub with git must exit 0; stderr: {}",
        o.stderr
    );
    let titles =
        parse_titles(&o).unwrap_or_else(|| panic!("list --json not an array: {}", o.stdout));
    assert!(
        titles.iter().any(|t| t == LEGACY_TITLE),
        "control: with git on PATH the legacy row for W1 must be listed from W1/sub; got {titles:?}"
    );
}

#[test]
fn without_git_legacy_path_unresolvable_list_is_undetermined_not_silently_partial() {
    let fx = fixture();
    let o = run(
        &fx.home,
        &fx.sub,
        "lister",
        &["discovery", "list", "--json"],
        false,
    );
    // Not a silent partial list: either it lists the legacy row (resolved
    // correctly) or it refuses. Per the contract it must refuse.
    let listed = parse_titles(&o);
    assert!(
        !matches!(&listed, Some(t) if !t.iter().any(|x| x == LEGACY_TITLE)),
        "without git, list from W1/sub must NOT print a list missing W1's legacy row \
         (silent partial / empty answer); exit {} stdout: {} stderr: {}",
        o.code,
        o.stdout,
        o.stderr
    );
    assert_ne!(
        o.code, 0,
        "without git the legacy path cannot be resolved: list must exit NON-ZERO; stdout: {} stderr: {}",
        o.stdout, o.stderr
    );
    assert!(
        o.stderr.to_lowercase().contains("undetermined"),
        "without git, list must say `undetermined` on stderr; stderr: {}",
        o.stderr
    );
}

// ---------------------------------------------------------------- gap

/// gap reads `<cwd>/.compass/{charter.md,opportunities.json}`, so seed them in
/// W1/sub; the opportunity's title matches the legacy row's fingerprint.
fn seed_gap(fx: &Fixture) {
    seed_compass_dir(&fx.base, &fx.sub, true);
}

fn gap(fx: &Fixture, git_on_path: bool) -> Value {
    let o = run(&fx.home, &fx.sub, "me", &["gap"], git_on_path);
    assert_eq!(
        o.code, 0,
        "gap must exit 0 (git_on_path={git_on_path}); stderr: {}",
        o.stderr
    );
    serde_json::from_str(&o.stdout).unwrap_or_else(|e| panic!("gap not JSON ({e}): {}", o.stdout))
}

fn has_opp(v: &Value) -> bool {
    v["opportunities"]
        .as_array()
        .expect("gap.opportunities is an array")
        .iter()
        .any(|o| o["title"] == LEGACY_TITLE)
}

#[test]
fn control_with_git_gap_dedups_against_legacy_row_and_says_applied() {
    let fx = fixture();
    seed_gap(&fx);
    let v = gap(&fx, true);
    assert_eq!(
        v["discovery_dedup"],
        serde_json::json!({ "status": "applied" }),
        "control: with git, dedup must be applied"
    );
    assert!(
        !has_opp(&v),
        "control: with git, the opportunity another session discovered (legacy row) must be \
         dropped; got {}",
        v["opportunities"]
    );
}

#[test]
fn without_git_gap_reports_dedup_undetermined_not_applied() {
    let fx = fixture();
    seed_gap(&fx);
    let v = gap(&fx, false);
    let dd = &v["discovery_dedup"];
    assert_eq!(
        dd["status"], "undetermined",
        "without git the legacy store cannot be located: discovery_dedup.status must be \
         \"undetermined\", not a claim that dedup ran; got {dd}"
    );
    assert!(
        dd["reason"].as_str().is_some_and(|s| !s.trim().is_empty()),
        "undetermined discovery_dedup must carry a non-empty reason; got {dd}"
    );
    assert!(
        has_opp(&v),
        "an undetermined dedup must keep the candidate; got {}",
        v["opportunities"]
    );
}

#[test]
fn sanity_w1_exists() {
    // Guard against a fixture whose worktree was never created (would make
    // every list trivially "absent").
    let fx = fixture();
    assert!(fx.w1.join(".git").is_file(), "W1 must be a linked worktree");
}
