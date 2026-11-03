// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Target HEAD provenance contract (C1/C5) of `fde ratify`, `fde evidence`
//! and `fde report`.
//!
//! Written by an independent test author (CLAUDE.md §2(a)); the implementer
//! must match the contract, not edit these tests to pass.
//!
//! Contract under test (coordinator-decided):
//!
//! 1. `fde ratify` records `head` (40 lowercase hex, `git rev-parse --verify
//!    HEAD` at the target root) and `dirty` (`git status --porcelain` printed
//!    anything; untracked included, ignored excluded) in `ratification.json`.
//!    Undeterminable (e.g. unborn HEAD) => nothing recorded, exit 2.
//! 2. Every evidence record carries `head` / `dirty` observed immediately
//!    BEFORE `CMD` runs. Undeterminable => nothing recorded, exit 2.
//! 3. Ancestry: with R = ratification head, C = current HEAD, H = a counted
//!    record's head: `evidence red|green|pass` needs R ancestor-or-equal of
//!    HEAD (else exit 2, nothing recorded); `verify` / `report` need R <= C
//!    and R <= H <= C for every counted record (else exit 2).
//! 4. dirty is shown, never disqualifying by itself; `report` prints the
//!    ratification head + clean/dirty, the current HEAD, and per requirement
//!    the record heads, marking dirty ones.
//! 5. Legacy ratification (no `head`/`dirty`): `status` unchanged, but
//!    `evidence *` / `verify` / `report` exit 2 telling the human to
//!    re-ratify. A counted ledger record without `head` => verify/report 2.
//!
//! `--cmd` values are free of shell operators (`true`, `false`, `test -f X`,
//! `touch X`). Toggle flags live outside the target (in the spec dir) so the
//! target tree is only dirtied where a test means to dirty it.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use common::{
    code, git, parse_resolved, run_fde, show, snapshot_files, stderr, stdout, write_spec,
    Engagement, TempPlace,
};

fn assert_code(out: &Output, want: i32, what: &str) {
    assert_eq!(code(out), Some(want), "{what}: {}", show(out));
}

/// Exit 2 with a non-empty stderr reason.
fn assert_undetermined(out: &Output, what: &str) {
    assert_code(out, 2, what);
    assert!(
        !stderr(out).trim().is_empty(),
        "{what}: exit 2 must state a reason on stderr: {}",
        show(out)
    );
}

fn ev(e: &Engagement, kind: &str, req: &str, cmd: &str) -> Output {
    e.fde(&["evidence", kind, "--req", req, "--cmd", cmd])
}

fn verify(e: &Engagement) -> Output {
    e.fde(&["evidence", "verify"])
}

fn report(e: &Engagement) -> Output {
    e.fde(&["report"])
}

/// A `test -f <flag>` command whose flag file lives outside the target.
fn flag_cmd(e: &Engagement, name: &str) -> (PathBuf, String) {
    let flag = e.spec_dir.path.join(name);
    let cmd = format!("test -f {}", flag.to_str().unwrap());
    (flag, cmd)
}

/// Record RED (flag absent) for `req`; returns (flag, cmd) for the GREEN.
fn red(e: &Engagement, req: &str) -> (PathBuf, String) {
    let (flag, cmd) = flag_cmd(e, &format!("flag-{req}"));
    assert_code(&ev(e, "red", req, &cmd), 0, &format!("RED {req}"));
    (flag, cmd)
}

/// Create the flag and record the GREEN matching [`red`].
fn green(e: &Engagement, req: &str, flag: &Path, cmd: &str) {
    fs::write(flag, "").unwrap();
    assert_code(&ev(e, "green", req, cmd), 0, &format!("GREEN {req}"));
}

fn red_then_green(e: &Engagement, req: &str) {
    let (flag, cmd) = red(e, req);
    green(e, req, &flag, &cmd);
}

/// `<state>/targets/<state_key>` of the engagement's target.
fn target_state_dir(e: &Engagement) -> PathBuf {
    let (_root, key) = parse_resolved(&e.fde(&["resolve"]));
    e.state.path.join("targets").join(key)
}

fn ratification_path(e: &Engagement) -> PathBuf {
    target_state_dir(e).join("ratification.json")
}

fn ledger_path(e: &Engagement) -> PathBuf {
    target_state_dir(e).join("evidence.json")
}

fn read_json(p: &Path) -> serde_json::Value {
    let bytes = fs::read(p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()));
    serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("parse {}: {e}", p.display()))
}

fn write_json(p: &Path, v: &serde_json::Value) {
    fs::write(p, serde_json::to_vec_pretty(v).unwrap()).unwrap();
}

/// `git rev-parse HEAD` at the target root (40 hex).
fn head_of(e: &Engagement) -> String {
    git(e.root(), &["rev-parse", "HEAD"]).trim().to_owned()
}

/// Commit a new file `name` in the target and return the new HEAD.
fn commit_file(e: &Engagement, name: &str) -> String {
    fs::write(e.root().join(name), format!("{name}\n")).unwrap();
    git(e.root(), &["add", name]);
    git(e.root(), &["commit", "-q", "-m", name]);
    head_of(e)
}

/// A commit with no parent, sharing nothing with the target's history, kept
/// reachable through `refs/heads/unrelated`. HEAD, index and worktree are
/// untouched.
fn unrelated_commit(e: &Engagement) -> String {
    let tree = git(e.root(), &["rev-parse", "HEAD^{tree}"])
        .trim()
        .to_owned();
    let id = git(e.root(), &["commit-tree", &tree, "-m", "unrelated root"])
        .trim()
        .to_owned();
    git(e.root(), &["update-ref", "refs/heads/unrelated", &id]);
    id
}

fn is_hex40(s: &str) -> bool {
    s.len() == 40
        && s.chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

/// `field` of the ratification JSON as a string / bool (panics with the JSON).
fn rat_head(v: &serde_json::Value) -> String {
    v.get("head")
        .and_then(|h| h.as_str())
        .unwrap_or_else(|| panic!("ratification.json lacks string \"head\": {v}"))
        .to_owned()
}

fn rat_dirty(v: &serde_json::Value) -> bool {
    v.get("dirty")
        .and_then(|d| d.as_bool())
        .unwrap_or_else(|| panic!("ratification.json lacks bool \"dirty\": {v}"))
}

/// The ledger's `records` array.
fn records(e: &Engagement) -> Vec<serde_json::Value> {
    let v = read_json(&ledger_path(e));
    v.get("records")
        .and_then(|r| r.as_array())
        .unwrap_or_else(|| panic!("evidence.json lacks array \"records\": {v}"))
        .clone()
}

fn rec_head(r: &serde_json::Value) -> String {
    r.get("head")
        .and_then(|h| h.as_str())
        .unwrap_or_else(|| panic!("evidence record lacks string \"head\": {r}"))
        .to_owned()
}

fn rec_dirty(r: &serde_json::Value) -> bool {
    r.get("dirty")
        .and_then(|d| d.as_bool())
        .unwrap_or_else(|| panic!("evidence record lacks bool \"dirty\": {r}"))
}

/// Lines of `text` that mention requirement `id` as a whole token.
fn lines_mentioning<'a>(text: &'a str, id: &str) -> Vec<&'a str> {
    text.lines()
        .filter(|l| {
            l.split(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
                .any(|tok| tok == id)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// (1) ratify records head + dirty
// ---------------------------------------------------------------------------

#[test]
fn ratify_on_clean_tree_records_head_and_dirty_false() {
    let e = Engagement::ratified(&[("R1", "first")]);
    let want = head_of(&e);
    assert!(is_hex40(&want), "fixture HEAD {want:?}");
    let v = read_json(&ratification_path(&e));
    assert_eq!(rat_head(&v), want, "ratification head must be HEAD: {v}");
    assert!(!rat_dirty(&v), "clean tree must record dirty=false: {v}");
}

#[test]
fn ratify_with_untracked_file_records_dirty_true() {
    let e = Engagement::new(&[("R1", "first")]);
    e.begin();
    fs::write(e.root().join("untracked.txt"), "x\n").unwrap();
    e.ratify();
    let v = read_json(&ratification_path(&e));
    assert_eq!(rat_head(&v), head_of(&e), "{v}");
    assert!(rat_dirty(&v), "untracked file => dirty=true: {v}");
    // dirty is not disqualifying: the target is ratified.
    assert_code(&e.fde(&["status"]), 0, "dirty ratification is ratified");
}

#[test]
fn ratify_with_modified_tracked_file_records_dirty_true() {
    let e = Engagement::new(&[("R1", "first")]);
    e.begin();
    fs::write(e.root().join("README.md"), "modified\n").unwrap();
    e.ratify();
    let v = read_json(&ratification_path(&e));
    assert_eq!(rat_head(&v), head_of(&e), "{v}");
    assert!(rat_dirty(&v), "modified tracked file => dirty=true: {v}");
}

#[test]
fn ratify_with_only_ignored_file_records_dirty_false() {
    let e = Engagement::new(&[("R1", "first")]);
    fs::write(e.root().join(".gitignore"), "*.log\n").unwrap();
    git(e.root(), &["add", ".gitignore"]);
    git(e.root(), &["commit", "-q", "-m", "ignore logs"]);
    e.begin();
    fs::write(e.root().join("build.log"), "noise\n").unwrap();
    // Control: git itself sees the tree as clean apart from the ignored file.
    assert_eq!(git(e.root(), &["status", "--porcelain"]), "", "control");
    e.ratify();
    let v = read_json(&ratification_path(&e));
    assert_eq!(rat_head(&v), head_of(&e), "{v}");
    assert!(!rat_dirty(&v), "ignored files do not make dirty: {v}");
}

// ---------------------------------------------------------------------------
// (2) unborn HEAD: ratify is undetermined and records nothing
// ---------------------------------------------------------------------------

#[test]
fn ratify_in_repo_without_commits_exits_2_and_writes_nothing() {
    let dir = TempPlace::new("fde-unborn-");
    let root = dir.path.clone();
    git(&root, &["init", "-q"]);
    let state = TempPlace::new("fde-state-");
    let spec_dir = TempPlace::new("fde-spec-");
    let spec = write_spec(&spec_dir.path, "spec.toml", "fixture", &[("R1", "first")]);
    let spec = spec.to_str().unwrap();

    // Registration does not need HEAD (begin probes only the top-level).
    let b = run_fde(&root, &state.path, &["begin", "--spec", spec]);
    assert_code(&b, 0, "begin in an unborn repo registers");

    let (_r, key) = parse_resolved(&run_fde(&root, &state.path, &["resolve"]));
    let rat = state
        .path
        .join("targets")
        .join(&key)
        .join("ratification.json");
    let before = snapshot_files(&state.path);

    let r = run_fde(&root, &state.path, &["ratify", "--spec", spec]);
    assert_undetermined(&r, "ratify with an unborn HEAD");
    assert!(
        !rat.exists(),
        "ratify with an undeterminable HEAD must not write {}",
        rat.display()
    );
    assert_eq!(
        before,
        snapshot_files(&state.path),
        "ratify with an unborn HEAD changed the state dir"
    );
    assert_code(
        &run_fde(&root, &state.path, &["status"]),
        1,
        "still unratified",
    );
}

// ---------------------------------------------------------------------------
// (3) evidence records carry head + dirty observed before CMD
// ---------------------------------------------------------------------------

#[test]
fn evidence_records_carry_head_and_dirty_of_record_time() {
    let e = Engagement::ratified(&[("R1", "first"), ("R2", "second")]);
    let a = head_of(&e);
    let (flag, cmd) = red(&e, "R1");

    let b = commit_file(&e, "between.txt");
    assert_ne!(a, b, "fixture: a new commit moves HEAD");
    // Dirty the tree before GREEN.
    fs::write(e.root().join("scratch.txt"), "wip\n").unwrap();
    green(&e, "R1", &flag, &cmd);

    let recs = records(&e);
    assert_eq!(recs.len(), 2, "RED + GREEN: {recs:?}");
    assert_eq!(rec_head(&recs[0]), a, "RED head is HEAD at RED time");
    assert!(!rec_dirty(&recs[0]), "RED taken on a clean tree");
    assert_eq!(rec_head(&recs[1]), b, "GREEN head is HEAD at GREEN time");
    assert!(rec_dirty(&recs[1]), "GREEN taken on a dirty tree");
    assert_ne!(rec_head(&recs[0]), rec_head(&recs[1]));

    // PASS record too.
    fs::remove_file(e.root().join("scratch.txt")).unwrap();
    assert_code(&ev(&e, "pass", "R2", "true"), 0, "PASS R2");
    let recs = records(&e);
    assert_eq!(recs.len(), 3);
    assert_eq!(rec_head(&recs[2]), b, "PASS head");
    assert!(!rec_dirty(&recs[2]), "PASS taken on a clean tree");
}

#[test]
fn evidence_dirty_is_observed_before_cmd_runs() {
    let e = Engagement::ratified(&[("R1", "first")]);
    // CMD itself dirties the target; the record must describe the tree it ran on.
    let made = e.root().join("made-by-cmd.txt");
    let cmd = format!("touch {}", made.to_str().unwrap());
    assert_code(&ev(&e, "pass", "R1", &cmd), 0, "PASS R1");
    assert!(made.exists(), "control: the command ran");
    let recs = records(&e);
    assert_eq!(recs.len(), 1);
    assert_eq!(rec_head(&recs[0]), head_of(&e));
    assert!(
        !rec_dirty(&recs[0]),
        "dirty must be observed BEFORE CMD (tree was clean then): {:?}",
        recs[0]
    );
}

// ---------------------------------------------------------------------------
// (4) forward history is explained divergence
// ---------------------------------------------------------------------------

#[test]
fn forward_history_between_ratify_red_and_green_is_accepted() {
    let e = Engagement::ratified(&[("R1", "first")]);
    let (flag, cmd) = red(&e, "R1");
    commit_file(&e, "forward.txt");
    green(&e, "R1", &flag, &cmd);
    assert_code(&verify(&e), 0, "R <= H <= C along forward history");
    assert_code(&report(&e), 0, "report done along forward history");
    // Moving further forward after GREEN is still fine.
    commit_file(&e, "later.txt");
    assert_code(&verify(&e), 0, "C moved forward after GREEN");
    assert_code(&report(&e), 0, "C moved forward after GREEN");
}

// ---------------------------------------------------------------------------
// (5) unexplained divergence: HEAD reset off the ratified line
// ---------------------------------------------------------------------------

#[test]
fn head_reset_to_non_descendant_makes_verify_report_and_recording_exit_2() {
    let e = Engagement::new(&[("R1", "first"), ("R2", "second")]);
    let parent = head_of(&e);
    let a = commit_file(&e, "second-commit.txt");
    e.begin();
    e.ratify();
    red_then_green(&e, "R1");
    red_then_green(&e, "R2");
    assert_code(&verify(&e), 0, "control: verify before the reset");
    assert_code(&report(&e), 0, "control: report before the reset");

    git(e.root(), &["reset", "-q", "--hard", &parent]);
    assert_code(&e.fde(&["status"]), 0, "status is about the spec only");

    let v = verify(&e);
    assert_undetermined(&v, "verify after HEAD left the ratified line");
    let r = report(&e);
    assert_undetermined(&r, "report after HEAD left the ratified line");

    let ledger = ledger_path(&e);
    let before = fs::read(&ledger).unwrap();
    for (kind, cmd) in [("red", "false"), ("green", "true"), ("pass", "true")] {
        let out = ev(&e, kind, "R1", cmd);
        assert_undetermined(&out, &format!("evidence {kind} after divergence"));
        let err = stderr(&out);
        assert!(
            err.contains(&a) || err.contains(&a[..12]),
            "evidence {kind}: stderr must name the ratification commit {a}: {}",
            show(&out)
        );
        assert_eq!(
            before,
            fs::read(&ledger).unwrap(),
            "evidence {kind} after divergence changed the ledger"
        );
    }
    // Fixture sanity (checked last so the exit-code contract above is what
    // fails first against an implementation without provenance).
    assert_eq!(rat_head(&read_json(&ratification_path(&e))), a);
}

// ---------------------------------------------------------------------------
// (6) ratification head unknown to the repo
// ---------------------------------------------------------------------------

#[test]
fn ratification_head_unknown_to_repo_makes_verify_and_report_exit_2() {
    let e = Engagement::ratified(&[("R1", "first")]);
    red_then_green(&e, "R1");
    assert_code(&verify(&e), 0, "control: verify");
    assert_code(&report(&e), 0, "control: report");

    let p = ratification_path(&e);
    let mut v = read_json(&p);
    assert!(is_hex40(&rat_head(&v)), "control: head recorded: {v}");
    v["head"] = serde_json::Value::String("0123456789abcdef0123456789abcdef01234567".into());
    write_json(&p, &v);
    assert_code(&e.fde(&["status"]), 0, "status is about the spec only");

    assert_undetermined(&verify(&e), "verify with an unknown ratification head");
    assert_undetermined(&report(&e), "report with an unknown ratification head");
}

// ---------------------------------------------------------------------------
// (7) counted record head off the R..C line
// ---------------------------------------------------------------------------

#[test]
fn counted_record_head_not_between_ratification_and_current_makes_verify_exit_2() {
    let e = Engagement::ratified(&[("R1", "first")]);
    red_then_green(&e, "R1");
    assert_code(&verify(&e), 0, "control: verify");
    let other = unrelated_commit(&e);

    let p = ledger_path(&e);
    let mut v = read_json(&p);
    let recs = v["records"].as_array_mut().unwrap();
    assert_eq!(recs.len(), 2, "{recs:?}");
    assert!(
        is_hex40(&rec_head(&recs[1])),
        "control: GREEN head recorded"
    );
    recs[1]["head"] = serde_json::Value::String(other.clone());
    write_json(&p, &v);

    assert_undetermined(&verify(&e), "verify with a GREEN head off the R..C line");
    assert_undetermined(&report(&e), "report with a GREEN head off the R..C line");
}

// ---------------------------------------------------------------------------
// (8) legacy ratification / legacy ledger record without head
// ---------------------------------------------------------------------------

#[test]
fn legacy_ratification_without_head_keeps_status_but_blocks_evidence_verify_report() {
    let e = Engagement::ratified(&[("R1", "first")]);
    red_then_green(&e, "R1");
    assert_code(&verify(&e), 0, "control: verify");
    assert_code(&report(&e), 0, "control: report");

    // Rewrite ratification.json in the previous schema (no head / dirty).
    let p = ratification_path(&e);
    let cur = read_json(&p);
    let legacy = serde_json::json!({
        "schema": 1,
        "spec": cur["spec"].clone(),
        "fingerprint": cur["fingerprint"].clone(),
    });
    assert!(legacy["spec"].is_string() && legacy["fingerprint"].is_string());
    write_json(&p, &legacy);

    assert_code(&e.fde(&["status"]), 0, "legacy ratification still ratified");

    let ledger = ledger_path(&e);
    let before = fs::read(&ledger).unwrap();
    let out = ev(&e, "red", "R1", "false");
    assert_undetermined(&out, "evidence red on a legacy ratification");
    assert!(
        stderr(&out).to_lowercase().contains("ratif"),
        "evidence red must tell the human to re-ratify: {}",
        show(&out)
    );
    assert_eq!(before, fs::read(&ledger).unwrap(), "nothing recorded");

    for (what, out) in [("verify", verify(&e)), ("report", report(&e))] {
        assert_undetermined(&out, &format!("{what} on a legacy ratification"));
        assert!(
            stderr(&out).to_lowercase().contains("ratif"),
            "{what} must tell the human to re-ratify: {}",
            show(&out)
        );
    }
}

#[test]
fn counted_ledger_record_without_head_makes_verify_and_report_exit_2() {
    let e = Engagement::ratified(&[("R1", "first")]);
    red_then_green(&e, "R1");
    assert_code(&verify(&e), 0, "control: verify");

    let p = ledger_path(&e);
    let mut v = read_json(&p);
    let recs = v["records"].as_array_mut().unwrap();
    assert_eq!(recs.len(), 2);
    let obj = recs[0].as_object_mut().unwrap();
    assert!(obj.remove("head").is_some(), "control: RED head recorded");
    obj.remove("dirty");
    write_json(&p, &v);

    assert_undetermined(&verify(&e), "verify with a head-less counted record");
    assert_undetermined(&report(&e), "report with a head-less counted record");
}

// ---------------------------------------------------------------------------
// (9) report text shows provenance
// ---------------------------------------------------------------------------

#[test]
fn report_shows_clean_ratification_head_and_current_head() {
    let e = Engagement::ratified(&[("R1", "first")]);
    let a = head_of(&e);
    red_then_green(&e, "R1");
    let b = commit_file(&e, "after-green.txt");
    let r = report(&e);
    assert_code(&r, 0, "complete along forward history");
    let text = stdout(&r);
    assert!(
        text.lines().any(|l| l.contains(&a) && l.contains("clean")),
        "report must show the ratification head {a} as clean:\n{text}"
    );
    assert!(
        text.contains(&b),
        "report must show the current HEAD {b}:\n{text}"
    );
    assert!(
        lines_mentioning(&text, "R1").iter().any(|l| l.contains(&a)),
        "R1's line must carry its record head {a}:\n{text}"
    );
    assert!(
        !text.contains("dirty"),
        "nothing was dirty; report must not say dirty:\n{text}"
    );
}

#[test]
fn report_marks_dirty_ratification_and_dirty_requirement_records() {
    let e = Engagement::new(&[("R1", "first"), ("R2", "second")]);
    e.begin();
    let untracked = e.root().join("wip.txt");
    fs::write(&untracked, "wip\n").unwrap();
    e.ratify();
    let a = head_of(&e);
    // R1 recorded on the dirty tree.
    red_then_green(&e, "R1");
    // R2 recorded on a clean tree.
    fs::remove_file(&untracked).unwrap();
    red_then_green(&e, "R2");

    let r = report(&e);
    assert_code(&r, 0, "dirty is shown, not disqualifying");
    let text = stdout(&r);
    assert!(
        text.lines().any(|l| l.contains(&a) && l.contains("dirty")),
        "report must show the ratification head {a} as dirty:\n{text}"
    );
    let r1 = lines_mentioning(&text, "R1");
    assert!(
        r1.iter().any(|l| l.contains("dirty")),
        "R1's records were taken on a dirty tree; its line must say dirty:\n{text}"
    );
    let r2 = lines_mentioning(&text, "R2");
    assert!(
        !r2.is_empty() && r2.iter().all(|l| !l.contains("dirty")),
        "R2's records were taken on a clean tree; its line must not say dirty:\n{text}"
    );
}
