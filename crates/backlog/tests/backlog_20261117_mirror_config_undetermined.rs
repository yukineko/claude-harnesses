#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! A `~/.backlog/config.toml` that EXISTS but cannot be read, or is not valid
//! TOML, silently turns the GitHub issue mirror back ON: `backlog done` runs
//! `gh issue close` and says nothing about the config.
//!
//! Observation (verbatim, measurement point rev cd09054b1, 2026-11-17):
//!   - `crates/backlog/src/config.rs` `GithubMirror::On` doc: "Key absent,
//!     `github_mirror = true`, or `config.toml` unreadable / not valid TOML
//!     (the pre-existing all-defaults path, unchanged)."
//!   - `GithubMirror::from_toml`: `let Ok(toml::Value::Table(table)) =
//!     toml::from_str::<toml::Value>(txt) else { return GithubMirror::On; };`
//!   - `Config::load`: `if let Ok(txt) = std::fs::read_to_string(&path) { ... }`
//!     — a read error leaves the initial `github_mirror: GithubMirror::On`.
//!   - README.ja.md documents the same thing: "なお config.toml 自体が読めない／
//!     TOML として不正な場合は、従来どおり全項目が既定値（ミラー ON）になる。"
//!
//! Expectation pinned here is CLAUDE.md §3, not the README: "判定不能 — IO 失敗
//! / ... / パース不能 ... — は clean ではない。必ず block か ask に解決する。"
//! A config file the operator wrote but that cannot be read/parsed means whether
//! `gh` may run is undetermined, exactly like the non-bool value that the same
//! module already refuses (`GithubMirror::Invalid`). So: no `gh` call, and an
//! error on stderr naming `config.toml`. NOTE the README states the opposite on
//! purpose ("従来どおり"), so this RED is a spec-vs-invariant contradiction that
//! needs a human ruling (CLAUDE.md §6 職務 1/2), not an impl-vs-spec bug.
//!
//! Two readings and how to tell them apart:
//!   (A) defect: an undetermined mirror setting falls open (gh runs). The RED
//!       tests fail on the "no gh" assertion with `issue close 901` in the log.
//!   (B) the fixture never reaches the inline close at all (so "gh ran" or
//!       "gh did not run" would mean nothing). Ruled out by the two controls:
//!       with NO config the same fixture closes issue 901 inline, and with a
//!       non-bool value it runs no gh and prints `error:` — i.e. the close path
//!       is reached and the refusal path exists for the neighbouring case.
//!
//! What this does NOT prove: it only drives `done` (the inline close via
//! `mirror_close_for`). `add` / `sync` / `cancel` / `recheck` read the same
//! `Config::load`, so they very likely share the fall-open, but that is not
//! observed here. It does not pin an exit code (README: `done` 等は issue を
//! 閉じずに `error:` を出す — no non-zero exit is promised for `done`). The
//! "directory named config.toml" case stands in for "unreadable"; a mode-000
//! file is not used because it is readable when the test runs as root.
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
        "bl-mirrorcfg-undet-{tag}-{}-{nanos}",
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

    fn cfg_path(&self) -> PathBuf {
        self.home.join(".backlog").join("config.toml")
    }

    fn write_cfg(&self, body: &str) {
        std::fs::create_dir_all(self.home.join(".backlog")).unwrap();
        std::fs::write(self.cfg_path(), body).unwrap();
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

/// Same shape as `github_mirror_disabled_close_20261117.rs`: linked worktree
/// on a work branch, GitHub-looking origin with a local bare pushurl, a stub
/// `gh` logging argv. `add` runs with NO config so issue 901 is filed; the
/// gh log is then emptied so only the close step is observed.
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
    git_ok(&repo, &["checkout", "-q", "-b", "work/mirror-cfg-undet"]);
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
    let title = format!(
        "mirrorcfg-undet-{tag}-{}",
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
    std::fs::write(&e.ghlog, "").unwrap();
    (e, id)
}

const DUP_TARGET: &str = "d0p0cafe";

fn run_done(e: &Env, id: &str) -> (i32, String, String) {
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
    e.bl(&["done", id, "--duplicate-of", DUP_TARGET])
}

/// The expectation shared by both RED cases: the local `done` happened, but
/// the undetermined mirror setting must not let `gh` run, and the operator
/// must be told on stderr that `config.toml` could not be used.
fn assert_undetermined_refuses(case: &str, e: &Env, id: &str, out: &(i32, String, String)) {
    let (c, o, er) = out;
    eprintln!("{case}: code={c} stdout={o:?} stderr={er:?}");
    assert!(
        o.contains(&format!("done: {id}")),
        "fixture: the local done must have happened: {out:?}"
    );
    let log = e.ghlog();
    assert!(
        !log.contains("issue close"),
        "{case}: whether the GitHub mirror may run cannot be determined (CLAUDE.md §3), \
         so `gh issue close` must not run; ghlog={log:?} stderr={er:?}"
    );
    assert!(
        er.lines()
            .any(|l| l.starts_with("error:") && l.contains("config.toml")),
        "{case}: an `error:` line naming config.toml must be printed; stderr={er:?}"
    );
    assert!(
        !e.store_text().contains("issue_closed_at"),
        "{case}: no close may be stamped: {}",
        e.store_text()
    );
}

// test-weakening-justified: new RED for an unfixed defect (2026-11-17).
#[test]
#[ignore = "RED: 2026-11-17 invalid-TOML config.toml turns the GitHub mirror ON (done runs gh issue close) — remove when fixed"]
fn red_done_with_invalid_toml_config_runs_no_gh_and_reports_error() {
    let (e, id) = setup_with_mirrored_row("badtoml");
    // The operator meant to turn the mirror OFF, but the file is broken.
    e.write_cfg("github_mirror = false\nthis is [[ not toml\n");
    let out = run_done(&e, &id);
    assert_undetermined_refuses("invalid TOML", &e, &id, &out);
}

// test-weakening-justified: new RED for an unfixed defect (2026-11-17).
#[test]
#[ignore = "RED: 2026-11-17 unreadable config.toml turns the GitHub mirror ON (done runs gh issue close) — remove when fixed"]
fn red_done_with_unreadable_config_runs_no_gh_and_reports_error() {
    let (e, id) = setup_with_mirrored_row("unreadable");
    // A directory where the file should be: read_to_string fails (EISDIR)
    // regardless of the uid the test runs as.
    std::fs::create_dir_all(e.cfg_path()).unwrap();
    assert!(std::fs::read_to_string(e.cfg_path()).is_err());
    let out = run_done(&e, &id);
    assert_undetermined_refuses("unreadable config.toml", &e, &id, &out);
}

/// Control (reading B): with no config the fixture really reaches the inline
/// close, so "gh ran" in the RED cases is the close path, not something else.
#[test]
fn control_done_without_config_closes_issue_inline() {
    let (e, id) = setup_with_mirrored_row("ctl-nocfg");
    let (c, o, er) = run_done(&e, &id);
    assert_eq!(c, 0, "done failed: stdout={o:?} stderr={er:?}");
    let log = e.ghlog();
    assert!(
        log.contains("issue close 901"),
        "control: done must close issue 901 inline; ghlog={log:?} stderr={er:?}"
    );
}

/// Control: the neighbouring undetermined case (present, non-bool value) is
/// already refused — the shape the RED cases expect exists in the binary.
#[test]
fn control_done_with_non_bool_value_runs_no_gh_and_errors() {
    let (e, id) = setup_with_mirrored_row("ctl-nonbool");
    e.write_cfg("github_mirror = \"no\"\n");
    let (c, o, er) = run_done(&e, &id);
    eprintln!("non-bool: code={c} stdout={o:?} stderr={er:?}");
    let log = e.ghlog();
    assert!(
        log.trim().is_empty(),
        "control: non-bool github_mirror must run no gh; ghlog={log:?}"
    );
    assert!(
        er.lines()
            .any(|l| l.starts_with("error:") && l.contains("config.toml")),
        "control: the error must name config.toml; stderr={er:?}"
    );
}
