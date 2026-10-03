#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `fugu-router harvest` — merges into `main` become episodes, labelled only
//! once the observation window has closed.
//!
//! Every spawned binary gets `HOME` = a scratch dir, so the real
//! `~/.fugu-router` is never touched. Fixtures are throwaway git repos with
//! explicit author/committer times.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

const EXE: &str = env!("CARGO_BIN_EXE_fugu-router");
const DAY: u64 = 86_400;
const OPUS: &str = "Co-Authored-By: Claude Opus 4 <noreply@anthropic.com>";
const SONNET: &str = "Co-Authored-By: Claude Sonnet 4.5 <noreply@anthropic.com>";

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn scratch(name: &str) -> PathBuf {
    let root =
        std::env::temp_dir().join(format!("fugu-harvest-test-{name}-{}", std::process::id()));
    if root.exists() {
        std::fs::remove_dir_all(&root).unwrap();
    }
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn git_at(dir: &Path, ts: u64, args: &[&str]) -> String {
    let date = format!("@{ts} +0000");
    let out = Command::new("git")
        .current_dir(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .env("HOME", dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

struct Fixture {
    root: PathBuf,
    repo: PathBuf,
    home: PathBuf,
}

type Commit<'a> = (u64, String, Vec<&'a str>);

impl Fixture {
    fn new(name: &str) -> Self {
        let root = scratch(name);
        let repo = root.join("repo");
        let home = root.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        git_at(&repo, now() - 60 * DAY, &["init", "-q", "-b", "main"]);
        let f = Fixture { root, repo, home };
        f.write("a.txt", "a0");
        f.write("b.txt", "b0");
        f.write("Cargo.toml", "v0");
        f.write("crates/x/.claude-plugin/plugin.json", "v0");
        git_at(&f.repo, now() - 59 * DAY, &["add", "-A"]);
        git_at(&f.repo, now() - 59 * DAY, &["commit", "-q", "-m", "init"]);
        f
    }

    fn write(&self, rel: &str, body: &str) {
        let p = self.repo.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    /// Branch off main with one commit per `(author_ts, message, files)`,
    /// then `--no-ff` merge it at `merge_ts`. Returns the merge SHA.
    fn merge(&self, branch: &str, subject: &str, merge_ts: u64, commits: &[Commit]) -> String {
        git_at(
            &self.repo,
            merge_ts,
            &["checkout", "-q", "-b", branch, "main"],
        );
        for (i, (ts, msg, files)) in commits.iter().enumerate() {
            for f in files {
                self.write(f, &format!("{branch}-{i}-{ts}"));
            }
            git_at(&self.repo, *ts, &["add", "-A"]);
            git_at(&self.repo, *ts, &["commit", "-q", "-m", msg]);
        }
        git_at(&self.repo, merge_ts, &["checkout", "-q", "main"]);
        git_at(
            &self.repo,
            merge_ts,
            &["merge", "-q", "--no-ff", "-m", subject, branch],
        );
        git_at(&self.repo, merge_ts, &["rev-parse", "HEAD"])
    }

    /// A plain commit directly on main.
    fn on_main(&self, ts: u64, subject: &str, files: &[&str]) {
        for f in files {
            self.write(f, &format!("main-{subject}-{ts}"));
        }
        git_at(&self.repo, ts, &["add", "-A"]);
        git_at(&self.repo, ts, &["commit", "-q", "-m", subject]);
    }

    fn harvest(&self, extra: &[&str]) -> Output {
        self.run_in(&self.repo, &["harvest"], extra)
    }

    fn run_in(&self, cwd: &Path, base: &[&str], extra: &[&str]) -> Output {
        Command::new(EXE)
            .current_dir(cwd)
            .args(base)
            .args(extra)
            .env("HOME", &self.home)
            .env_remove("FUGU_ROUTER_MODE")
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    fn episodes(&self) -> Vec<serde_json::Value> {
        let p = self.home.join(".fugu-router").join("episodes.jsonl");
        match std::fs::read_to_string(p) {
            Ok(s) => s
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(|l| serde_json::from_str(l).unwrap())
                .collect(),
            Err(_) => vec![],
        }
    }

    fn fr_dir(&self) -> PathBuf {
        self.home.join(".fugu-router")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn ok(o: &Output) {
    assert!(
        o.status.success(),
        "harvest exited {:?}\nstdout: {}\nstderr: {}",
        o.status.code(),
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
}

fn msg(subject: &str, trailer: Option<&str>) -> String {
    match trailer {
        Some(t) => format!("{subject}\n\n{t}"),
        None => subject.to_string(),
    }
}

/// One mature merge touching a.txt/new.txt/Cargo.toml/plugin.json (merge at
/// now-10d) followed by a commit on main with `fix_subject` touching
/// `fix_files` at merge+`fix_off`. Returns the single recorded episode.
fn pass_after(
    name: &str,
    fix_subject: &str,
    fix_files: &[&str],
    fix_off: u64,
) -> serde_json::Value {
    let fx = Fixture::new(name);
    let t = now() - 10 * DAY;
    fx.merge(
        "feat",
        "Merge feat",
        t,
        &[(
            t - 3600,
            msg("feat: a", Some(OPUS)),
            vec![
                "a.txt",
                "new.txt",
                "Cargo.toml",
                "crates/x/.claude-plugin/plugin.json",
            ],
        )],
    );
    fx.on_main(t + fix_off, fix_subject, fix_files);
    // fixture fact: the fix commit is on main after the merge
    let top = git_at(&fx.repo, t, &["log", "-1", "--format=%s", "main"]);
    assert_eq!(top, fix_subject);
    ok(&fx.harvest(&[]));
    let eps = fx.episodes();
    assert_eq!(eps.len(), 1, "expected exactly one episode, got {eps:?}");
    eps[0].clone()
}

#[test]
fn mature_merge_is_recorded_with_correct_fields() {
    let fx = Fixture::new("fields");
    let t = now() - 10 * DAY;
    let sha = fx.merge(
        "feat",
        "Merge feat: add thing",
        t,
        &[(
            t - 7200,
            msg("feat: add thing", Some(OPUS)),
            vec!["a.txt", "new.txt"],
        )],
    );
    // fixture fact
    let log = git_at(
        &fx.repo,
        t,
        &[
            "log",
            "--first-parent",
            "--merges",
            "--format=%H %s",
            "main",
        ],
    );
    assert_eq!(log, format!("{sha} Merge feat: add thing"));
    ok(&fx.harvest(&[]));
    let eps = fx.episodes();
    assert_eq!(eps.len(), 1, "{eps:?}");
    let e = &eps[0];
    assert_eq!(e["class"], "merge");
    assert_eq!(e["role"], "worker");
    assert_eq!(e["model"], "opus");
    assert_eq!(e["pass"], true);
    assert_eq!(e["title"], "Merge feat: add thing");
    let mut files: Vec<String> = e["touched_files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    files.sort();
    assert_eq!(files, vec!["a.txt", "new.txt"]);
    assert_eq!(e["duration_secs"].as_f64(), Some(7200.0));
}

#[test]
fn immature_merge_is_not_recorded_until_window_closes() {
    let fx = Fixture::new("immature");
    let t = now() - DAY;
    fx.merge(
        "feat",
        "Merge young",
        t,
        &[(t - 60, msg("feat", Some(OPUS)), vec!["a.txt"])],
    );
    ok(&fx.harvest(&[]));
    assert!(
        fx.episodes().is_empty(),
        "merge younger than window was recorded"
    );
    // not marked harvested: a later run with a shorter window picks it up
    ok(&fx.harvest(&["--window-days", "0"]));
    assert_eq!(
        fx.episodes().len(),
        1,
        "immature merge was marked harvested too early"
    );
}

#[test]
fn merge_older_than_lookback_is_not_recorded() {
    let fx = Fixture::new("lookback");
    let t = now() - 20 * DAY;
    fx.merge(
        "feat",
        "Merge old",
        t,
        &[(t - 60, msg("feat", Some(OPUS)), vec!["a.txt"])],
    );
    ok(&fx.harvest(&["--lookback-days", "10"]));
    assert!(fx.episodes().is_empty());
}

#[test]
fn fix_overlapping_within_window_marks_fail() {
    let e = pass_after("fixfail", "fix: broken a", &["a.txt"], DAY);
    assert_eq!(e["pass"], false);
}

#[test]
fn fix_subject_variants_that_count_and_that_do_not() {
    for (i, (subj, expect_pass)) in [
        ("Fix: upper case", false),
        ("fix(core): scoped", false),
        ("Revert \"feat: a\"", false),
        ("revert: plain", false),
        ("fixture: not a fix", true),
        ("reverted nothing", true),
    ]
    .into_iter()
    .enumerate()
    {
        let e = pass_after(&format!("subj-{i}"), subj, &["a.txt"], DAY);
        assert_eq!(e["pass"], expect_pass, "subject {subj:?}");
    }
}

#[test]
fn fix_touching_only_noise_paths_is_still_pass() {
    let e = pass_after(
        "noise",
        "fix: bump",
        &["Cargo.toml", "crates/x/.claude-plugin/plugin.json"],
        DAY,
    );
    assert_eq!(
        e["pass"], true,
        "noise-only overlap must not fail the merge"
    );
}

#[test]
fn fix_outside_window_is_pass() {
    let e = pass_after("outside", "fix: late", &["a.txt"], 4 * DAY);
    assert_eq!(e["pass"], true);
}

#[test]
fn non_fix_commit_overlapping_is_pass() {
    let e = pass_after("nonfix", "feat: more a", &["a.txt"], DAY);
    assert_eq!(e["pass"], true);
}

#[test]
fn fix_touching_unrelated_file_is_pass() {
    let e = pass_after("unrelated", "fix: other", &["b.txt"], DAY);
    assert_eq!(e["pass"], true);
}

#[test]
fn merge_without_trailer_is_skipped_but_marked_harvested() {
    let fx = Fixture::new("notrailer");
    let t = now() - 10 * DAY;
    let sha = fx.merge(
        "feat",
        "Merge plain",
        t,
        &[(t - 60, msg("feat: plain", None), vec!["a.txt"])],
    );
    ok(&fx.harvest(&[]));
    assert!(fx.episodes().is_empty(), "no trailer => no episode");
    let cursor = std::fs::read_to_string(fx.fr_dir().join("harvest-cursor.json"))
        .expect("harvest-cursor.json must exist: skipped merges are still marked harvested");
    assert!(
        cursor.contains(&sha),
        "cursor lacks skipped merge {sha}: {cursor}"
    );
}

#[test]
fn majority_trailer_model_wins() {
    let fx = Fixture::new("majority");
    let t = now() - 10 * DAY;
    fx.merge(
        "feat",
        "Merge multi",
        t,
        &[
            (t - 300, msg("c1", Some(SONNET)), vec!["a.txt"]),
            (t - 200, msg("c2", Some(OPUS)), vec!["b.txt"]),
            (
                t - 100,
                msg("c3", Some("Co-authored-by: Claude OPUS 4.1 <n@a>")),
                vec!["new.txt"],
            ),
        ],
    );
    ok(&fx.harvest(&[]));
    let eps = fx.episodes();
    assert_eq!(eps.len(), 1);
    assert_eq!(eps[0]["model"], "opus");
    assert_eq!(
        eps[0]["duration_secs"].as_f64(),
        Some(300.0),
        "earliest author time is t-300"
    );
}

#[test]
fn second_run_records_nothing_new() {
    let fx = Fixture::new("idem");
    let t = now() - 10 * DAY;
    fx.merge(
        "feat",
        "Merge once",
        t,
        &[(t - 60, msg("feat", Some(OPUS)), vec!["a.txt"])],
    );
    ok(&fx.harvest(&[]));
    assert_eq!(fx.episodes().len(), 1);
    ok(&fx.harvest(&[]));
    assert_eq!(fx.episodes().len(), 1, "harvest is not idempotent");
}

#[test]
fn corrupt_cursor_fails_loudly_surfaces_in_prompt_and_clears_after_success() {
    let fx = Fixture::new("corrupt");
    let t = now() - 10 * DAY;
    fx.merge(
        "feat",
        "Merge c",
        t,
        &[(t - 60, msg("feat", Some(OPUS)), vec!["a.txt"])],
    );
    std::fs::create_dir_all(fx.fr_dir()).unwrap();
    let cursor = fx.fr_dir().join("harvest-cursor.json");
    std::fs::write(&cursor, "{ this is not json").unwrap();

    let out = fx.harvest(&[]);
    assert!(
        !out.status.success(),
        "corrupt cursor must not be treated as empty"
    );
    assert!(
        fx.episodes().is_empty(),
        "no episodes may be recorded on a corrupt cursor"
    );
    assert!(
        fx.fr_dir().join("harvest-error.json").exists(),
        "harvest-error.json marker missing"
    );
    assert_eq!(
        std::fs::read_to_string(&cursor).unwrap(),
        "{ this is not json",
        "the corrupt cursor must not be overwritten"
    );

    let payload = r#"{"session_id":"s1","prompt":"refactor the parser in src/lib.rs"}"#;
    let prompt = |fx: &Fixture| {
        use std::io::Write;
        let mut c = Command::new(EXE)
            .arg("prompt")
            .env("HOME", &fx.home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        c.stdin
            .take()
            .unwrap()
            .write_all(payload.as_bytes())
            .unwrap();
        String::from_utf8_lossy(&c.wait_with_output().unwrap().stdout).to_string()
    };
    assert!(
        prompt(&fx).to_lowercase().contains("harvest"),
        "prompt hook did not surface the harvest failure"
    );

    std::fs::remove_file(&cursor).unwrap();
    ok(&fx.harvest(&[]));
    assert_eq!(fx.episodes().len(), 1);
    assert!(
        !fx.fr_dir().join("harvest-error.json").exists(),
        "a successful harvest must clear harvest-error.json"
    );
    assert!(
        !prompt(&fx).to_lowercase().contains("harvest"),
        "stale harvest notice survived a successful harvest"
    );
}

#[test]
fn non_git_cwd_exits_zero_and_records_nothing() {
    let fx = Fixture::new("nogit");
    let plain = fx.root.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let out = fx.run_in(&plain, &["harvest"], &[]);
    ok(&out);
    assert!(fx.episodes().is_empty());
}

#[test]
fn explicit_repo_flag_works_from_another_cwd() {
    let fx = Fixture::new("repoflag");
    let t = now() - 10 * DAY;
    fx.merge(
        "feat",
        "Merge r",
        t,
        &[(t - 60, msg("feat", Some(OPUS)), vec!["a.txt"])],
    );
    let elsewhere = fx.root.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let out = fx.run_in(
        &elsewhere,
        &["harvest", "--repo", fx.repo.to_str().unwrap()],
        &[],
    );
    ok(&out);
    assert_eq!(fx.episodes().len(), 1);
}

#[test]
fn master_branch_is_used_when_main_is_absent() {
    let fx = Fixture::new("master");
    let t = now() - 10 * DAY;
    fx.merge(
        "feat",
        "Merge m",
        t,
        &[(t - 60, msg("feat", Some(OPUS)), vec!["a.txt"])],
    );
    git_at(&fx.repo, t, &["branch", "-m", "main", "master"]);
    ok(&fx.harvest(&[]));
    assert_eq!(fx.episodes().len(), 1);
}

// ---- R1: `.backlog/` bookkeeping is noise ------------------------------------

/// One mature merge touching a.txt and `.backlog/tasks.toml`, then a fix commit
/// on main touching `fix_files` a day later. Returns the single episode.
fn pass_after_backlog_merge(name: &str, fix_files: &[&str]) -> serde_json::Value {
    let fx = Fixture::new(name);
    let t = now() - 10 * DAY;
    fx.merge(
        "feat",
        "Merge feat",
        t,
        &[(
            t - 3600,
            msg("feat: a", Some(OPUS)),
            vec!["a.txt", ".backlog/tasks.toml"],
        )],
    );
    fx.on_main(t + DAY, "fix: follow-up", fix_files);
    let top = git_at(&fx.repo, t, &["log", "-1", "--format=%s", "main"]);
    assert_eq!(top, "fix: follow-up");
    ok(&fx.harvest(&[]));
    let eps = fx.episodes();
    assert_eq!(eps.len(), 1, "expected exactly one episode, got {eps:?}");
    eps[0].clone()
}

#[test]
fn fix_overlapping_only_on_backlog_files_is_still_pass() {
    // control: a fix overlapping on a real file still fails the merge
    let real = pass_after_backlog_merge("backlog-ctl", &["a.txt"]);
    assert_eq!(real["pass"], false, "control: real-file overlap must fail");
    // a fix overlapping on a real file AND .backlog still fails
    let both = pass_after_backlog_merge("backlog-both", &["a.txt", ".backlog/tasks.toml"]);
    assert_eq!(both["pass"], false, "real-file overlap alongside .backlog");

    for (i, f) in [".backlog/tasks.toml", ".backlog/sub/dir/other.json"]
        .into_iter()
        .enumerate()
    {
        let e = pass_after_backlog_merge(&format!("backlog-only-{i}"), &[f]);
        assert_eq!(
            e["pass"], true,
            "overlap only on {f} (bookkeeping under .backlog/) must not fail the merge"
        );
    }
}

// ---- R2': self-sync merges (subject names the branch itself) are skipped -----

fn cursor_text(fx: &Fixture) -> String {
    std::fs::read_to_string(fx.fr_dir().join("harvest-cursor.json"))
        .expect("harvest-cursor.json must exist")
}

#[test]
fn pushed_pull_sync_merge_is_skipped_and_marked_but_work_merge_is_recorded() {
    let fx = Fixture::new("pushedsync");
    let origin = fx.root.join("origin.git");
    let clone2 = fx.root.join("clone2");
    std::fs::create_dir_all(&origin).unwrap();
    git_at(&origin, now(), &["init", "-q", "--bare", "-b", "main"]);
    let origin_s = origin.to_str().unwrap();
    git_at(&fx.repo, now(), &["remote", "add", "origin", origin_s]);
    git_at(&fx.repo, now(), &["push", "-q", "-u", "origin", "main"]);

    let t_work = now() - 20 * DAY;
    let work = fx.merge(
        "feat",
        "Merge feat: real work",
        t_work,
        &[(t_work - 60, msg("feat: real", Some(OPUS)), vec!["a.txt"])],
    );
    git_at(&fx.repo, t_work, &["push", "-q", "origin", "main"]);

    git_at(
        &fx.root,
        now(),
        &["clone", "-q", origin_s, clone2.to_str().unwrap()],
    );
    std::fs::write(clone2.join("c.txt"), "theirs").unwrap();
    let t_their = now() - 14 * DAY;
    git_at(&clone2, t_their, &["add", "-A"]);
    git_at(
        &clone2,
        t_their,
        &["commit", "-q", "-m", &msg("feat: theirs", Some(OPUS))],
    );
    git_at(&clone2, t_their, &["push", "-q", "origin", "main"]);

    fx.on_main(now() - 13 * DAY, "local: mine", &["b.txt"]);
    let t_sync = now() - 12 * DAY;
    git_at(
        &fx.repo,
        t_sync,
        &["pull", "-q", "--no-rebase", "--no-edit"],
    );
    let sync = git_at(&fx.repo, t_sync, &["rev-parse", "HEAD"]);
    git_at(&fx.repo, t_sync, &["push", "-q", "origin", "main"]);

    // fixture facts: the sync merge is a merge, has been pushed, and names main
    let parents = git_at(&fx.repo, t_sync, &["rev-list", "--parents", "-1", &sync]);
    assert_eq!(parents.split_whitespace().count(), 3, "must be a merge");
    assert_eq!(
        git_at(&fx.repo, t_sync, &["rev-parse", "origin/main"]),
        sync,
        "fixture: the sync merge must have been pushed"
    );
    let subj = git_at(&fx.repo, t_sync, &["log", "-1", "--format=%s", &sync]);
    assert!(subj.contains("'main'"), "fixture: subject was {subj:?}");

    ok(&fx.harvest(&[]));
    let titles: Vec<String> = fx
        .episodes()
        .iter()
        .map(|e| e["title"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        titles,
        vec!["Merge feat: real work".to_string()],
        "only the work merge may be recorded; the pushed pull-sync merge must be skipped"
    );
    let cursor = cursor_text(&fx);
    assert!(cursor.contains(&work), "cursor lacks work merge {work}");
    assert!(
        cursor.contains(&sync),
        "skipped sync merge {sync} must still be marked harvested: {cursor}"
    );
    ok(&fx.harvest(&[]));
    assert_eq!(fx.episodes().len(), 1, "second run changed the store");
}

#[test]
fn self_sync_subjects_are_skipped_and_work_subjects_recorded() {
    let cases: [(&str, bool); 7] = [
        ("Merge branch 'main' of https://x/y", true),
        ("Merge remote-tracking branch 'origin/main'", true),
        ("Merge origin/main (autoflow x)", true),
        (
            "merge: integrate origin/main 81e102fd into session-b9a78b46",
            true,
        ),
        ("Merge session-40aa606d — x", false),
        ("Merge flow-34385c9e/bg-del-impl — y", false),
        ("Merge feature 'mainline-x'", false),
    ];
    for (i, (subject, skipped)) in cases.into_iter().enumerate() {
        let fx = Fixture::new(&format!("subj-sync-{i}"));
        let t = now() - 10 * DAY;
        let sha = fx.merge(
            "side",
            subject,
            t,
            &[(t - 60, msg("feat: x", Some(OPUS)), vec!["a.txt"])],
        );
        ok(&fx.harvest(&[]));
        if skipped {
            assert!(
                fx.episodes().is_empty(),
                "subject {subject:?} names the branch itself: must be skipped"
            );
            assert!(
                cursor_text(&fx).contains(&sha),
                "skipped merge {subject:?} must still be marked harvested"
            );
        } else {
            assert_eq!(
                fx.episodes().len(),
                1,
                "subject {subject:?} is a work merge: must be recorded"
            );
        }
    }
}

#[test]
fn self_sync_subject_uses_the_actual_branch_name_master() {
    let fx = Fixture::new("master-sync");
    let t = now() - 10 * DAY;
    let sha = fx.merge(
        "side",
        "Merge branch 'master' of https://x/y",
        t,
        &[(t - 60, msg("feat: x", Some(OPUS)), vec!["a.txt"])],
    );
    git_at(&fx.repo, t, &["branch", "-m", "main", "master"]);
    ok(&fx.harvest(&[]));
    assert!(
        fx.episodes().is_empty(),
        "'master' sync merge on a master repo must be skipped"
    );
    assert!(cursor_text(&fx).contains(&sha), "must be marked harvested");
}

// ---------------------------------------------------------------------------
// Which stream the messages go to. `harvest` is run by the same `SessionEnd`
// hook as `sync` (`hooks/hooks.json`: `fugu-router harvest; fugu-router sync`),
// and a SessionEnd hook's stderr is surfaced to the user while its stdout is
// discarded. So the stream is the user-visible behaviour, not a detail.
//
// Measured 2026-10-02 at rev b478fdcf (fugu-router 0.1.31): the success line
// `fugu-router: harvested {n} merge episode(s)` was an `eprintln!`
// (`src/harvest.rs:51`), so a harvest that worked perfectly printed to stderr
// and showed up as an error at the end of a session. The fix is a split, not a
// silencing: success/progress on stdout, stderr reserved for failures —
// silencing both would convert a noisy success into an invisible failure, the
// fail-open this repository forbids (CLAUDE.md §1/§3).
// ---------------------------------------------------------------------------

/// A harvest that succeeded must put NOTHING on stderr — and must still report
/// what it harvested, on stdout. Both halves are asserted on purpose: an empty
/// stderr alone would be satisfied by a command that went mute.
#[test]
fn a_successful_harvest_is_silent_on_stderr_and_reports_on_stdout() {
    let fx = Fixture::new("stream-success");
    let t = now() - 10 * DAY;
    fx.merge(
        "feat",
        "Merge feat: add thing",
        t,
        &[(
            t - 7200,
            msg("feat: add thing", Some(OPUS)),
            vec!["a.txt", "new.txt"],
        )],
    );

    let out = fx.harvest(&[]);
    ok(&out);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();

    // Precondition: there is something to report. The success line is only
    // printed when n > 0, so a harvest of zero episodes would make the stdout
    // assertion below vacuous.
    let eps = fx.episodes();
    assert_eq!(
        eps.len(),
        1,
        "precondition failed: expected exactly one harvested episode, got \
         {eps:?}\nstdout: {stdout:?}\nstderr: {stderr:?}"
    );

    assert_eq!(
        stderr.trim(),
        "",
        "a SUCCESSFUL `fugu-router harvest` wrote to stderr. It runs from the \
         SessionEnd hook, whose stderr is surfaced to the user, so this text is \
         part of why a clean session end looked like an error. stderr must be \
         reserved for failures.\n\
         observed stderr: {stderr:?}\nobserved stdout: {stdout:?}"
    );

    assert!(
        stdout.contains("harvested 1 merge episode(s)"),
        "harvest's success line is on neither stream. It must move to stdout — \
         discarded at SessionEnd, but still informative when a human runs \
         `fugu-router harvest` by hand — not disappear.\n\
         observed stdout: {stdout:?}\nobserved stderr: {stderr:?}"
    );
}
