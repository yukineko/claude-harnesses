//! RED repro (not fixed this round): `compass gap` and `compass route` turn an
//! unreadable opportunity store into ZERO candidates.
//!
//! `crates/compass/src/main.rs` (gap ~700, route ~960):
//!     opportunity::list_under_ranked(&root, &charter.north_star).unwrap_or_default()
//! `opportunity::load` returns `Err` when `<root>/.compass/opportunities.json`
//! cannot be read (e.g. it is a directory) or does not parse as JSON; the
//! `unwrap_or_default()` erases that into `vec![]`, so `gap` prints
//! `"opportunities": []` — indistinguishable from "no bets recorded" — and exits 0.
//!
//! Required: a store that cannot be read is NOT reported as zero opportunities.
//! Either the command exits non-zero, or its output says the opportunity store
//! is `undetermined`. A silent `[]` with exit 0 fails.
//!
//! Control (not ignored): a readable store with one opportunity is listed by
//! `gap`, so the RED cases fail because of the unreadable store only.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

const CHARTER_JSON: &str = r#"{"north_star":"ship it","definition_of_done":["tests pass"],"measuring_stick":"tests passing","current_gap":"gap","next_action":"act","parked":[]}"#;
const OPP_TITLE: &str = "opportunity in a store under test";

struct Fixture {
    _tmp: tempfile::TempDir,
    r: PathBuf,
    w1: PathBuf,
    home: PathBuf,
}

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

fn git(cwd: &Path, args: &[&str]) {
    let out = Command::new("git")
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

fn run(home: &Path, cwd: &Path, args: &[&str], stdin: Option<&str>) -> Out {
    use std::io::Write;
    let mut child = Command::new(env!("CARGO_BIN_EXE_compass"))
        .args(args)
        .env("HOME", home)
        .env("CLAUDE_CODE_SESSION_ID", "s1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env_remove("HARNESS_SANDBOX_HOME")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .current_dir(cwd)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("compass spawns");
    if let Some(input) = stdin {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
    }
    let out = child.wait_with_output().expect("compass runs");
    Out {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// Repo R (charter committed) + linked worktree W1; a valid opportunities.json
/// holding [`OPP_TITLE`] is placed in both.
fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let r = base.join("repo");
    let home = base.join("home");
    let scratch = base.join("scratch");
    let scratch_home = base.join("scratch-home");
    for d in [&r, &home, &scratch, &scratch_home] {
        std::fs::create_dir_all(d).unwrap();
    }
    let o = run(
        &scratch_home,
        &scratch,
        &["charter", "--write", CHARTER_JSON],
        None,
    );
    assert_eq!(o.code, 0, "SETUP: charter seed failed: {}", o.stderr);
    let o = run(
        &scratch_home,
        &scratch,
        &["opportunity", "add", "--title", OPP_TITLE],
        None,
    );
    assert_eq!(o.code, 0, "SETUP: opportunity add failed: {}", o.stderr);

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
    git(
        &r,
        &["worktree", "add", "-q", "-b", "f1", w1.to_str().unwrap()],
    );
    for root in [&r, &w1] {
        std::fs::copy(
            scratch.join(".compass/opportunities.json"),
            root.join(".compass/opportunities.json"),
        )
        .unwrap();
    }
    Fixture {
        _tmp: tmp,
        r,
        w1,
        home,
    }
}

enum Breakage {
    /// `opportunities.json` replaced by a directory (read error).
    Directory,
    /// `opportunities.json` is not valid JSON (parse error).
    CorruptJson,
}

fn break_store(root: &Path, how: Breakage) {
    let p = root.join(".compass/opportunities.json");
    match how {
        Breakage::Directory => {
            std::fs::remove_file(&p).unwrap();
            std::fs::create_dir(&p).unwrap();
            std::fs::write(p.join("blocker"), "x").unwrap();
        }
        Breakage::CorruptJson => std::fs::write(&p, "{ this is not json").unwrap(),
    }
}

/// The silent-erasure shape: exit 0, `opportunities` is an empty array, and
/// nothing anywhere says the store was undetermined.
fn gap_silently_reports_zero(o: &Out) -> bool {
    if o.code != 0 {
        return false;
    }
    let said_undetermined = format!("{}\n{}", o.stdout, o.stderr)
        .to_lowercase()
        .contains("undetermined");
    let empty = matches!(
        serde_json::from_str::<Value>(&o.stdout).ok().as_ref().map(|v| &v["opportunities"]),
        Some(Value::Array(a)) if a.is_empty()
    );
    empty && !said_undetermined
}

#[test]
fn control_readable_store_gap_lists_the_opportunity() {
    let fx = fixture();
    let o = run(&fx.home, &fx.r, &["gap"], None);
    assert_eq!(o.code, 0, "control: gap must exit 0; stderr: {}", o.stderr);
    let v: Value = serde_json::from_str(&o.stdout).expect("gap prints JSON");
    assert!(
        v["opportunities"]
            .as_array()
            .expect("opportunities array")
            .iter()
            .any(|x| x["title"] == OPP_TITLE),
        "control: a readable store's opportunity must be listed; got {}",
        v["opportunities"]
    );
}

#[test]
fn control_readable_store_route_lists_the_opportunity_ref() {
    let fx = fixture();
    let dec = r#"{"goal":"g","tasks":[{"id":"t1","title":"do it","size":"m"}]}"#;
    let o = run(&fx.home, &fx.w1, &["route"], Some(dec));
    assert_eq!(
        o.code, 0,
        "control: route must exit 0; stderr: {}",
        o.stderr
    );
    assert!(
        o.stdout.contains(OPP_TITLE),
        "control: route must list a readable store's opportunity ref; stdout: {}",
        o.stdout
    );
    assert!(
        !format!("{}\n{}", o.stdout, o.stderr)
            .to_lowercase()
            .contains("undetermined"),
        "control: a healthy route must not mention `undetermined` (else the RED check is vacuous); \
         stdout: {} stderr: {}",
        o.stdout,
        o.stderr
    );
}

#[test]
#[ignore = "RED repro: backlog 2b6db3a7; gap/route unwrap_or_default on unreadable opportunity store"]
fn gap_with_opportunity_store_as_directory_does_not_report_zero_opportunities() {
    let fx = fixture();
    break_store(&fx.r, Breakage::Directory);
    let o = run(&fx.home, &fx.r, &["gap"], None);
    assert!(
        !gap_silently_reports_zero(&o),
        "unreadable opportunity store (a directory) was reported as zero opportunities: \
         exit {} stdout: {} stderr: {}",
        o.code,
        o.stdout,
        o.stderr
    );
}

#[test]
#[ignore = "RED repro: backlog 2b6db3a7; gap/route unwrap_or_default on unreadable opportunity store"]
fn gap_with_corrupt_opportunity_store_does_not_report_zero_opportunities() {
    let fx = fixture();
    break_store(&fx.r, Breakage::CorruptJson);
    let o = run(&fx.home, &fx.r, &["gap"], None);
    assert!(
        !gap_silently_reports_zero(&o),
        "corrupt opportunity store was reported as zero opportunities: \
         exit {} stdout: {} stderr: {}",
        o.code,
        o.stdout,
        o.stderr
    );
}

#[test]
#[ignore = "RED repro: backlog 2b6db3a7; gap/route unwrap_or_default on unreadable opportunity store"]
fn route_with_corrupt_opportunity_store_does_not_silently_drop_opportunity_refs() {
    // route refuses the primary checkout, so run it from linked worktree W1.
    let fx = fixture();
    break_store(&fx.w1, Breakage::CorruptJson);
    let dec = r#"{"goal":"g","tasks":[{"id":"t1","title":"do it","size":"m"}]}"#;
    let o = run(&fx.home, &fx.w1, &["route"], Some(dec));
    let said_undetermined = format!("{}\n{}", o.stdout, o.stderr)
        .to_lowercase()
        .contains("undetermined");
    assert!(
        o.code != 0 || said_undetermined,
        "route with a corrupt opportunity store exited 0 without saying the store was \
         undetermined (opportunity refs silently empty): stdout: {} stderr: {}",
        o.stdout,
        o.stderr
    );
}
