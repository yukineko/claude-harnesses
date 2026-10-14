//! backlog 016c2562: a task tagged `wontfix` (a human ruling: "do not
//! implement") must never be handed out by `next` / `next --claim`, while the
//! other pending tasks in the same queue keep being handed out normally.
//!
//! Written by an independent test author (not the implementer). Each test
//! fixes the wontfix task as the one the ordinary queue order would pick
//! (higher priority, added first), so selection that ignores tags hands it
//! out and the test goes RED. The positive half (the normal task IS handed
//! out, rc 0) keeps the tests from passing on a `next` that errors or returns
//! nothing. `control_untagged_p1_is_picked_over_p3` proves the fixture's
//! ordering premise independently of the tag.
//!
//! Every fixture is a fresh temp git repo with an isolated HOME: neither the
//! real `~/.backlog` nor this repo's `.backlog` is touched.

mod common;

use std::path::PathBuf;
use std::process::{Command, Stdio};

fn unique(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "backlog-016c2562-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    // The binary resolves identity through canonical paths; macOS temp is a symlink.
    std::fs::canonicalize(&dir).unwrap()
}

/// A fresh git repo (the store follows the checkout) plus an isolated HOME.
struct Fx {
    home: PathBuf,
    repo: PathBuf,
}

fn fx(tag: &str) -> Fx {
    let root = unique(tag);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    common::linked_checkout(&repo);
    // `add` lands `pending` only with a REPRODUCED repro test from a committed
    // script (otherwise `unconfirmed`, which `next` never hands out — that
    // would make every test here vacuous). Commit one.
    std::fs::create_dir_all(repo.join("tests")).unwrap();
    std::fs::write(
        repo.join("tests/repro_yes.sh"),
        "echo 'bug present'; exit 1\n",
    )
    .unwrap();
    for args in [
        &["add", "tests/repro_yes.sh"][..],
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t.t",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            "repro",
        ],
    ] {
        let ok = Command::new("git")
            .args(args)
            .current_dir(&repo)
            .status()
            .expect("git runs")
            .success();
        assert!(ok, "git {args:?} failed: fixture is void");
    }
    Fx { home, repo }
}

fn run(f: &Fx, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_backlog"))
        .args(args)
        .env("HOME", &f.home)
        .env("PATH", common::path_with_condukt_shim())
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(&f.repo)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Add a pending task and return its id (precondition: add must succeed).
fn add(f: &Fx, title: &str, extra: &[&str]) -> String {
    let proj = f.repo.to_str().unwrap();
    let mut args = vec![
        "add",
        "--title",
        title,
        "--project",
        proj,
        "--repro-test",
        "bash tests/repro_yes.sh",
    ];
    args.extend_from_slice(extra);
    let (rc, out, err) = run(f, &args);
    assert_eq!(rc, 0, "precondition: add must succeed; out={out} err={err}");
    out.lines()
        .find_map(|l| l.strip_prefix("added: "))
        .unwrap_or_else(|| panic!("no 'added:' line in {out:?}"))
        .trim()
        .to_string()
}

/// The wontfix task: highest priority and added first, so tag-blind
/// selection picks it.
fn add_wontfix(f: &Fx) -> String {
    add(
        f,
        "ruled out work",
        &["--priority", "p1", "--tag", "wontfix"],
    )
}

/// A normal task that tag-blind selection ranks BELOW the wontfix one.
fn add_normal(f: &Fx) -> String {
    add(f, "ordinary work", &["--priority", "p3"])
}

/// Parse the task `next` printed and return its `id`. Panics (test fails) when
/// stdout is not a task — e.g. `no pending tasks` or an error.
fn handed_out_id(cmd: &str, rc: i32, out: &str, err: &str) -> String {
    assert_eq!(
        rc, 0,
        "`{cmd}` must succeed when a non-wontfix pending task exists; out={out:?} err={err:?}"
    );
    let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap_or_else(|e| {
        panic!("`{cmd}` did not hand out a task (stdout is not task JSON: {e}); out={out:?} err={err:?}")
    });
    v.get("id")
        .and_then(|x| x.as_str())
        .unwrap_or_else(|| panic!("`{cmd}` task JSON has no string id: {out:?}"))
        .to_string()
}

/// Status of `id` as `list --json` reports it (derived claim view included).
fn listed_status(f: &Fx, id: &str) -> String {
    let (rc, out, err) = run(f, &["list", "--json"]);
    assert_eq!(
        rc, 0,
        "precondition: list --json must succeed; out={out:?} err={err:?}"
    );
    let v: serde_json::Value = serde_json::from_str(out.trim())
        .unwrap_or_else(|e| panic!("list --json is not JSON ({e}): {out:?}"));
    let row = v
        .as_array()
        .unwrap_or_else(|| panic!("list --json is not an array: {out:?}"))
        .iter()
        .find(|t| t.get("id").and_then(|x| x.as_str()) == Some(id))
        .unwrap_or_else(|| panic!("task {id} missing from list --json: {out:?}"));
    row.get("status")
        .and_then(|x| x.as_str())
        .unwrap_or_else(|| panic!("row for {id} has no status: {row:?}"))
        .to_string()
}

/// Control (green before and after the fix): the fixture's ordering premise.
/// Without the tag, the p1 task added first IS what `next --claim` hands out,
/// so the wontfix tests below are only satisfiable by honouring the tag.
#[test]
fn control_untagged_p1_is_picked_over_p3() {
    let f = fx("control");
    let p1 = add(&f, "ruled out work", &["--priority", "p1"]);
    let _p3 = add_normal(&f);
    let (rc, out, err) = run(&f, &["next", "--claim"]);
    assert_eq!(handed_out_id("next --claim", rc, &out, &err), p1);
}

/// (a) `next --claim` with a wontfix task ranked first and a normal task:
/// succeeds and claims the normal task; the wontfix task stays unclaimed.
#[test]
fn next_claim_skips_wontfix_and_claims_the_normal_task() {
    let f = fx("claim-mixed");
    let wf = add_wontfix(&f);
    let normal = add_normal(&f);

    let (rc, out, err) = run(&f, &["next", "--claim"]);
    let got = handed_out_id("next --claim", rc, &out, &err);
    assert_ne!(
        got, wf,
        "next --claim handed out wontfix-tagged task {wf}; out={out:?} err={err:?}"
    );
    assert_eq!(
        got, normal,
        "next --claim must hand out the non-wontfix pending task {normal}; out={out:?}"
    );
    assert_ne!(
        listed_status(&f, &wf),
        "claimed",
        "wontfix-tagged task {wf} is claimed after next --claim"
    );
}

/// (b) Only a wontfix task in the queue: `next --claim` must not return it,
/// and it must not end up claimed.
#[test]
fn next_claim_never_claims_a_lone_wontfix_task() {
    let f = fx("claim-lone");
    let wf = add_wontfix(&f);
    assert_eq!(
        listed_status(&f, &wf),
        "pending",
        "precondition: the wontfix task must be pending before next --claim"
    );

    let (_rc, out, err) = run(&f, &["next", "--claim"]);
    assert!(
        !out.contains(&wf),
        "next --claim handed out the lone wontfix-tagged task {wf}; out={out:?} err={err:?}"
    );
    assert_ne!(
        listed_status(&f, &wf),
        "claimed",
        "lone wontfix-tagged task {wf} was claimed by next --claim; out={out:?} err={err:?}"
    );
}

/// Plain `next` (read-only) with a wontfix task ranked first and a normal
/// task: offers the normal task, not the wontfix one.
#[test]
fn next_skips_wontfix_and_offers_the_normal_task() {
    let f = fx("next-mixed");
    let wf = add_wontfix(&f);
    let normal = add_normal(&f);

    let (rc, out, err) = run(&f, &["next"]);
    let got = handed_out_id("next", rc, &out, &err);
    assert_ne!(
        got, wf,
        "next offered wontfix-tagged task {wf}; out={out:?} err={err:?}"
    );
    assert_eq!(
        got, normal,
        "next must offer the non-wontfix pending task {normal}; out={out:?}"
    );
}

/// Plain `next` with only a wontfix task: must not offer it.
#[test]
fn next_never_offers_a_lone_wontfix_task() {
    let f = fx("next-lone");
    let wf = add_wontfix(&f);
    let (_rc, out, err) = run(&f, &["next"]);
    assert!(
        !out.contains(&wf),
        "next offered the lone wontfix-tagged task {wf}; out={out:?} err={err:?}"
    );
}
