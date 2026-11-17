#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `github_mirror = false` must also stop the INLINE close that `done` /
//! `cancel` / `recheck` (auto-cancel) run through `mirror_close_for`.
//!
//! Written by an independent verifier (CLAUDE.md §2(a)) for the inline-close
//! path that `github_mirror_disabled_20261117.rs` does not cover.
//!
//! Shape of every test: `add` runs with NO config (mirror ON), so the stub
//! `gh` files issue 901 and the row carries `issue_number`. Only then is
//! `$HOME/.backlog/config.toml` written, and the close command runs. So the
//! row is one that `mirror_close_for` WOULD close; whether `gh issue close`
//! runs is read from the stub's argv log.
//!
//! Unlike `done_closes_issue_inline.rs`, the binary gets explicit
//! GIT_AUTHOR_* / GIT_COMMITTER_* (with env_clear() and no global git config,
//! `add`'s land step otherwise dies with "Author identity unknown").
mod common;

use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn tool_path(name: &str) -> PathBuf {
    for d in std::env::var("PATH").unwrap().split(':') {
        let p = PathBuf::from(d).join(name);
        if p.exists() {
            return p;
        }
    }
    panic!("no {name} on PATH");
}

fn uniq(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "bl-mirroroff-close-{tag}-{}-{nanos}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn git_ok(cwd: &Path, args: &[&str]) -> String {
    let o = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("git runs");
    assert!(
        o.status.success(),
        "git {args:?} in {} failed: {}",
        cwd.display(),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn write_exec(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

struct Env {
    home: PathBuf,
    bin: PathBuf,
    repo: PathBuf,
    ghlog: PathBuf,
}

impl Env {
    fn bl(&self, args: &[&str]) -> (i32, String, String) {
        let o = Command::new(env!("CARGO_BIN_EXE_backlog"))
            .args(args)
            .current_dir(&self.repo)
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", &self.bin)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .unwrap();
        (
            o.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&o.stdout).into_owned(),
            String::from_utf8_lossy(&o.stderr).into_owned(),
        )
    }

    fn ghlog(&self) -> String {
        std::fs::read_to_string(&self.ghlog).unwrap_or_default()
    }

    fn write_cfg(&self, body: Option<&str>) {
        let dir = self.home.join(".backlog");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("config.toml");
        match body {
            Some(b) => std::fs::write(p, b).unwrap(),
            None => {
                let _ = std::fs::remove_file(p);
            }
        }
    }

    fn store_text(&self) -> String {
        let mut s = String::new();
        for f in ["tasks.toml", "tasks.done.toml"] {
            s.push_str(
                &std::fs::read_to_string(self.repo.join(".backlog").join(f)).unwrap_or_default(),
            );
        }
        s
    }
}

/// Linked worktree on a work branch, `remote.origin.url` reads as GitHub,
/// pushes go to a local bare repo (same shape as
/// `github_mirror_disabled_20261117.rs`). Then `add` one row with NO config,
/// so the stub files issue 901 for it. Returns the env and the row id.
fn setup_with_mirrored_row(tag: &str) -> (Env, String) {
    let root = uniq(tag);
    let (home, bin, repo, bare, ghlog) = (
        root.join("home"),
        root.join("bin"),
        root.join("repo"),
        root.join("origin.git"),
        root.join("gh.log"),
    );
    for d in [&home, &bin] {
        std::fs::create_dir_all(d).unwrap();
    }
    common::linked_checkout(&repo);
    let primary = root.join("repo.main");
    git_ok(&repo, &["checkout", "-q", "-b", "work/mirror-off-close"]);
    common::commit_repro(&repo);

    std::fs::create_dir_all(&bare).unwrap();
    git_ok(&bare, &["init", "-q", "--bare", "-b", "main"]);
    git_ok(&primary, &["push", "-q", bare.to_str().unwrap(), "main"]);
    git_ok(
        &primary,
        &["remote", "add", "origin", "https://github.com/o/r.git"],
    );
    git_ok(
        &primary,
        &["config", "remote.origin.pushurl", bare.to_str().unwrap()],
    );

    for t in ["git", "grep", "cat", "sh"] {
        symlink(tool_path(t), bin.join(t)).unwrap();
    }
    symlink("/bin/bash", bin.join("bash")).unwrap();
    write_exec(
        &bin.join("condukt"),
        "#!/bin/sh\necho '{\"claimed\":false}'\nexit 1\n",
    );
    let _ = Command::new(bin.join("condukt"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    write_exec(
        &bin.join("gh"),
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {log}\n\
             if [ \"$1\" = issue ] && [ \"$2\" = view ]; then echo '{{\"state\":\"OPEN\"}}'; fi\n\
             if [ \"$1\" = issue ] && [ \"$2\" = create ]; then echo https://github.com/o/r/issues/901; fi\n\
             exit 0\n",
            log = ghlog.display()
        ),
    );

    let e = Env {
        home,
        bin,
        repo,
        ghlog,
    };
    // Mirror ON for the add (no config file).
    e.write_cfg(None);
    let title = format!(
        "mirroroff-close-{tag}-{}",
        root.file_name().unwrap().to_string_lossy()
    );
    let (c, o, er) = e.bl(&[
        "add",
        "--title",
        &title,
        "--project",
        ".",
        "--repro-test",
        common::COMMITTED_REPRO,
    ]);
    assert_eq!(c, 0, "fixture add failed: stdout={o:?} stderr={er:?}");
    let id = o
        .lines()
        .find_map(|l| l.strip_prefix("added: "))
        .expect("add must print the new id")
        .trim()
        .to_string();
    let log = e.ghlog();
    assert_eq!(
        log.matches("issue create").count(),
        1,
        "fixture: add with mirror ON must file exactly one issue; ghlog={log:?}"
    );
    assert!(
        e.store_text().contains("issue_number = 901"),
        "fixture: the row must carry the issue the close would target: {}",
        e.store_text()
    );
    // Start the close step with an empty log.
    std::fs::write(&e.ghlog, "").unwrap();
    (e, id)
}

/// Close-evidence route for `done`: `--duplicate-of` a canonical row seeded
/// as `done` with no issue (same as `done_closes_issue_inline.rs`).
const DUP_TARGET: &str = "d0p0cafe";

fn seed_duplicate_target(e: &Env) {
    let p = e.repo.join(".backlog/tasks.done.toml");
    let block = format!(
        "[[task]]\nid = \"{DUP_TARGET}\"\ntitle = \"canonical ticket\"\nproject = \"/repo\"\ntags = []\nstatus = \"done\"\nnotes = \"\"\ncreated_at = 1\nupdated_at = 1\nweight = 0.0\n\n"
    );
    let mut cur = std::fs::read_to_string(&p).unwrap_or_default();
    if !cur.contains(DUP_TARGET) {
        cur.push_str(&block);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, cur).unwrap();
    }
}

fn run_done(e: &Env, id: &str) -> (i32, String, String) {
    seed_duplicate_target(e);
    e.bl(&["done", id, "--duplicate-of", DUP_TARGET])
}

/// Make the committed repro stop reproducing (exit 0) at HEAD, so `recheck`
/// classifies the row not-reproduced and AUTO-CANCELS it (-> mirror_close_for).
fn make_repro_stop_reproducing(e: &Env) {
    let p = e.repo.join("tests/backlog_fixture_repro.sh");
    write_exec(&p, "#!/bin/bash\necho 'fixed'\nexit 0\n");
    git_ok(
        &e.repo,
        &["add", "-f", "--", "tests/backlog_fixture_repro.sh"],
    );
    git_ok(
        &e.repo,
        &["commit", "-q", "-m", "repro no longer reproduces"],
    );
}

fn says_mirror_disabled(s: &str) -> bool {
    s.lines().any(|l| {
        let l = l.to_lowercase();
        l.contains("mirror") && l.contains("disabled")
    })
}

fn assert_no_close_and_disabled_line(cmd: &str, e: &Env, out: &(i32, String, String)) {
    let (c, o, er) = out;
    let log = e.ghlog();
    assert!(
        !log.contains("issue close"),
        "{cmd} with github_mirror = false must not run `gh issue close`; ghlog={log:?} \
         stdout={o:?} stderr={er:?}"
    );
    assert!(
        log.trim().is_empty(),
        "{cmd} with github_mirror = false must not run gh at all; ghlog={log:?}"
    );
    assert_eq!(
        *c, 0,
        "{cmd} itself must succeed: stdout={o:?} stderr={er:?}"
    );
    let disabled: Vec<&str> = er.lines().filter(|l| says_mirror_disabled(l)).collect();
    assert_eq!(
        disabled.len(),
        1,
        "{cmd} must say on exactly one line that the mirror is disabled; stderr={er:?}"
    );
    assert!(
        !e.store_text().contains("issue_closed_at"),
        "no close ran, so nothing may be stamped as closed: {}",
        e.store_text()
    );
}

#[test]
fn done_with_mirror_false_does_not_close_issue_and_says_so() {
    let (e, id) = setup_with_mirrored_row("done-off");
    e.write_cfg(Some("github_mirror = false\n"));
    let out = run_done(&e, &id);
    assert_no_close_and_disabled_line("done", &e, &out);
}

#[test]
fn cancel_with_mirror_false_does_not_close_issue_and_says_so() {
    let (e, id) = setup_with_mirrored_row("cancel-off");
    e.write_cfg(Some("github_mirror = false\n"));
    let out = e.bl(&["cancel", &id, "--reason", "not needed"]);
    assert!(
        out.1.contains(&format!("cancelled: {id}")),
        "cancel must have cancelled the row: {out:?}"
    );
    assert_no_close_and_disabled_line("cancel", &e, &out);
}

#[test]
fn recheck_autocancel_with_mirror_false_does_not_close_issue_and_says_so() {
    let (e, id) = setup_with_mirrored_row("recheck-off");
    make_repro_stop_reproducing(&e);
    e.write_cfg(Some("github_mirror = false\n"));
    let out = e.bl(&["recheck", "--id", &id]);
    assert!(
        out.1.contains(&format!("\tcancelled\t{id}")),
        "fixture: recheck must AUTO-CANCEL the row (that is the path that closes): {out:?}"
    );
    assert_no_close_and_disabled_line("recheck", &e, &out);
}

/// Control: without the key, `done` closes the issue inline (the path the
/// tests above disable is really reached by this fixture).
#[test]
fn control_done_without_key_closes_issue() {
    let (e, id) = setup_with_mirrored_row("done-ctl");
    e.write_cfg(None);
    let (c, o, er) = run_done(&e, &id);
    assert_eq!(c, 0, "done failed: stdout={o:?} stderr={er:?}");
    let log = e.ghlog();
    assert!(
        log.contains("issue close 901 --reason completed"),
        "control: done must close issue 901 inline; ghlog={log:?} stderr={er:?}"
    );
}

/// Control for the recheck fixture: without the key the auto-cancel closes.
#[test]
fn control_recheck_autocancel_without_key_closes_issue() {
    let (e, id) = setup_with_mirrored_row("recheck-ctl");
    make_repro_stop_reproducing(&e);
    let out = e.bl(&["recheck", "--id", &id]);
    assert!(
        out.1.contains(&format!("\tcancelled\t{id}")),
        "fixture: recheck must AUTO-CANCEL the row: {out:?}"
    );
    let log = e.ghlog();
    assert!(
        log.contains("issue close 901"),
        "control: recheck auto-cancel must close issue 901 inline; ghlog={log:?} out={out:?}"
    );
}

/// Non-bool value: whether `gh` may run is undetermined -> no gh, and the
/// key is named on stderr as an error. (Exit code is NOT pinned here: the
/// local `done` has already been committed when the close step runs.)
#[test]
fn done_with_non_bool_mirror_value_runs_no_gh_and_names_the_key() {
    let (e, id) = setup_with_mirrored_row("done-nonbool");
    e.write_cfg(Some("github_mirror = \"no\"\n"));
    let (c, o, er) = run_done(&e, &id);
    eprintln!("done(nonbool) code={c} stdout={o:?} stderr={er:?}");
    let log = e.ghlog();
    assert!(
        log.trim().is_empty(),
        "non-bool github_mirror: done must not run gh at all; ghlog={log:?}"
    );
    assert!(
        er.lines()
            .any(|l| l.starts_with("error:") && l.contains("github_mirror")),
        "non-bool github_mirror must be reported as an error naming the key; stderr={er:?}"
    );
    assert!(
        !e.store_text().contains("issue_closed_at"),
        "no close ran: {}",
        e.store_text()
    );
}
