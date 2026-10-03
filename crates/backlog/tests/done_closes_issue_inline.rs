#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `backlog done` must close the task's GitHub issue INLINE, not leave it for a
//! later `backlog sync`.
//!
//! This path (`main.rs`'s `mirror_close_for`, reached from the `Done` handler)
//! had NO test at all — `grep -rln mirror_close_for crates/backlog` hit only
//! `src/main.rs`, measured 2026-10-02 at measurement point `a3cf25fc`. It is
//! also the exact mechanism that has to work for a finished task to stop
//! showing as an open issue, and the one whose silent absence let 467 closes
//! accumulate: on this machine `gh` was missing, so every inline close
//! degraded, and nothing downstream was watching.
//!
//! The reconciliation run of 2026-10-02 proved `decide_issue_close` + `gh_probe`
//! work against real GitHub (462 confirmed closes, verified with `gh issue
//! view`), but `mirror_close_for` differs from `sync` in building its own
//! one-item plan after re-reading the store, and that difference was unpinned.
//!
//! Drives the real binary against a stub `gh` that records every argv, so what
//! is asserted is the GitHub-visible write actually attempted.
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;

fn tool_path(name: &str) -> PathBuf {
    for d in std::env::var("PATH").unwrap().split(':') {
        let p = PathBuf::from(d).join(name);
        if p.exists() {
            return p;
        }
    }
    panic!("no {name} on PATH");
}

struct Env {
    home: PathBuf,
    bin: PathBuf,
    repo: PathBuf,
    ghlog: PathBuf,
}

fn setup(tag: &str) -> Env {
    let t = std::env::temp_dir().join(format!("bl-doneclose-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&t);
    let (home, bin, repo) = (t.join("home"), t.join("bin"), t.join("repo"));
    for d in [&home, &bin, &repo] {
        std::fs::create_dir_all(d).unwrap();
    }
    symlink(tool_path("git"), bin.join("git")).unwrap();
    // The stub shells out to `grep`, and the binary runs with env_clear() +
    // PATH=bin, so the stub's own tools have to live in that same dir.
    symlink(tool_path("grep"), bin.join("grep")).unwrap();

    let ghlog = t.join("gh.log");
    let stub = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {log}\n\
         if [ \"$1\" = issue ] && [ \"$2\" = create ]; then\n\
         \u{20} n=$(grep -c 'issue create' {log})\n\
         \u{20} echo \"https://github.com/o/r/issues/$((700 + n))\"\n\
         fi\nexit 0\n",
        log = ghlog.display()
    );
    std::fs::write(bin.join("gh"), stub).unwrap();
    std::fs::set_permissions(
        bin.join("gh"),
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
    )
    .unwrap();

    let g = |a: &[&str]| {
        assert!(Command::new("git")
            .args(a)
            .current_dir(&repo)
            .env("PATH", std::env::var("PATH").unwrap())
            .env("HOME", &home)
            .status()
            .unwrap()
            .success());
    };
    g(&["init", "-q", "."]);
    g(&["remote", "add", "origin", "https://github.com/o/r.git"]);

    Env {
        home,
        bin,
        repo,
        ghlog,
    }
}

/// Close-evidence (main 04ea9b35): a bare `backlog done ID` is refused, so the
/// close these tests mirror goes through the cheapest evidence route,
/// `--duplicate-of`, naming a canonical row seeded here as `done` with NO
/// issue. That row contributes nothing to any sync plan (`sync_plan` only acts
/// on a terminal row that HOLDS an unclosed issue), so every count asserted
/// below is about the task under test alone.
const DUP_TARGET: &str = "d0p0cafe";

fn seed_duplicate_target(done_file: &Path) {
    let block = format!(
        "[[task]]\nid = \"{DUP_TARGET}\"\ntitle = \"canonical ticket\"\nproject = \"/repo\"\ntags = []\nstatus = \"done\"\nnotes = \"\"\ncreated_at = 1\nupdated_at = 1\nweight = 0.0\n\n"
    );
    let mut cur = std::fs::read_to_string(done_file).unwrap_or_default();
    if !cur.contains(DUP_TARGET) {
        cur.push_str(&block);
        std::fs::create_dir_all(done_file.parent().unwrap()).unwrap();
        std::fs::write(done_file, cur).unwrap();
    }
}

fn bl(e: &Env, args: &[&str]) -> (i32, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_backlog"))
        .args(args)
        .current_dir(&e.repo)
        .env_clear()
        .env("HOME", &e.home)
        .env("PATH", &e.bin)
        .output()
        .unwrap();
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

fn ghlog(e: &Env) -> String {
    std::fs::read_to_string(&e.ghlog).unwrap_or_default()
}

fn done_file(e: &Env) -> String {
    std::fs::read_to_string(e.repo.join(".backlog/tasks.done.toml")).unwrap_or_default()
}

/// Add (issue filed by the stub) then `done` must close THAT issue number and
/// stamp the confirmation, leaving nothing for a later `sync` to pick up.
///
/// Dies if the `Done` handler stops calling `mirror_close_for`, if it closes a
/// different number, if it closes with the wrong `--reason`, or if it stamps
/// `issue_closed_at` without a confirmed close (the stamp and the gh call are
/// asserted separately for exactly that reason).
#[test]
fn done_closes_the_issue_inline_and_records_it() {
    let e = setup("ok");
    let (c, o, er) = bl(&e, &["add", "--title", "inline close me", "--project", "."]);
    assert_eq!(c, 0, "add failed: {er}");
    let id = o
        .lines()
        .find_map(|l| l.strip_prefix("added: "))
        .expect("add must print the new id")
        .trim()
        .to_string();
    let log = ghlog(&e);
    assert_eq!(
        log.matches("issue create").count(),
        1,
        "add must file exactly one issue; ghlog={log:?}"
    );
    // The number the stub handed back is the one `done` has to close.
    let number = 701;

    seed_duplicate_target(&e.repo.join(".backlog/tasks.done.toml"));
    let (c, o, er) = bl(&e, &["done", &id, "--duplicate-of", DUP_TARGET]);
    eprintln!("done code={c} stdout={o:?} stderr={er:?}");
    assert_eq!(c, 0, "done failed: {er}");

    let log = ghlog(&e);
    assert!(
        log.contains(&format!("issue close {number} --reason completed")),
        "done must close the task's own issue as completed, inline; ghlog={log:?}"
    );
    assert_eq!(
        log.matches("issue close").count(),
        1,
        "exactly one close; ghlog={log:?}"
    );

    let done = done_file(&e);
    assert!(
        done.contains("issue_closed_at"),
        "a confirmed inline close must be stamped, so `sync` does not redo it: {done}"
    );

    // Nothing left over: the whole point of closing inline is that the mirror
    // is already reconciled by the time the command returns.
    let (c, o, er) = bl(&e, &["sync"]);
    assert_eq!(c, 0, "{er}");
    assert!(
        o.contains("0 issue(s) to close"),
        "an inline close must leave no residual sync work: {o:?}"
    );
}

/// THE FAIL-CLOSED HALF. When `gh` cannot close the issue, `done` must still
/// record the local completion (the task really is finished) but must NOT stamp
/// `issue_closed_at` — otherwise the open issue becomes invisible to every
/// later `sync` and nothing ever revisits it. This is the asymmetry
/// `CloseOutcome` exists to express: "could not close" is not "closed".
///
/// Dies if a failed close is ever recorded as a close, and if the failure is
/// swallowed without a word on either stream.
#[test]
fn a_failed_inline_close_is_not_recorded_and_stays_in_the_sync_plan() {
    let e = setup("fail");
    let (c, o, er) = bl(&e, &["add", "--title", "close will fail", "--project", "."]);
    assert_eq!(c, 0, "add failed: {er}");
    let id = o
        .lines()
        .find_map(|l| l.strip_prefix("added: "))
        .expect("add must print the new id")
        .trim()
        .to_string();

    // Swap the stub for one that fails every close but still logs the attempt.
    let stub = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {log}\nexit 1\n",
        log = e.ghlog.display()
    );
    std::fs::write(e.bin.join("gh"), stub).unwrap();
    std::fs::set_permissions(
        e.bin.join("gh"),
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
    )
    .unwrap();

    seed_duplicate_target(&e.repo.join(".backlog/tasks.done.toml"));
    let (c, o, er) = bl(&e, &["done", &id, "--duplicate-of", DUP_TARGET]);
    eprintln!("done code={c} stdout={o:?} stderr={er:?}");
    assert_eq!(
        c, 0,
        "the local completion is authoritative and must succeed"
    );
    assert!(
        ghlog(&e).contains("issue close"),
        "the close must have been attempted: {:?}",
        ghlog(&e)
    );
    let done = done_file(&e);
    assert!(
        !done.contains("issue_closed_at"),
        "an UNCONFIRMED close must not be stamped: {done}"
    );
    assert!(
        format!("{o}{er}").to_lowercase().contains("sync")
            || format!("{o}{er}").to_lowercase().contains("issue"),
        "a failed mirror close must not be silent: stdout={o:?} stderr={er:?}"
    );

    // And the retry survives: sync still plans the close.
    let (c, o, er) = bl(&e, &["sync"]);
    assert_eq!(c, 0, "{er}");
    assert!(
        o.contains("1 issue(s) to close"),
        "an unconfirmed close must still be in the next sync plan: {o:?}"
    );
}
