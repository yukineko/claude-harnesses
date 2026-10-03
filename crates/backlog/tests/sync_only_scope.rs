#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `backlog sync --only {create,close,both}`: the two mirror arms are
//! selectable, because they do not have the same blast radius.
//!
//! Measured 2026-10-02 at measurement point `448ff46f`, this repo's own store:
//! `backlog sync` planned **58 creates and 467 closes**. The 467 closes are
//! bookkeeping catch-up on issues that already exist on GitHub; the 58 creates
//! would PUBLISH 58 brand-new public issues. The operator's ruling was to
//! reconcile the closes and leave the pending tasks unmirrored — which the
//! single `--apply` switch made impossible to express, since the plan is
//! ordered by the store and `--limit` truncates across both kinds.
//!
//! These tests drive the real binary against a stub `gh` that records every
//! argv it is handed, so what is asserted is the set of GitHub-visible writes
//! actually attempted — not an internal predicate that could agree with a
//! broken filter.
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
    /// Every argv the stub `gh` was invoked with, one line per invocation.
    ghlog: PathBuf,
}

/// A repo with a github.com origin, a `.backlog` store in a known drifted
/// shape, and a stub `gh` on PATH that records its argv.
///
/// The store is written by hand rather than built through `add`/`done` on
/// purpose: `done` closes the mirror inline, so there is no way to reach the
/// "done, issue exists, never confirmed closed" shape through the CLI once the
/// mirror works. That shape is exactly what 467 of this repo's tasks are in.
fn setup(tag: &str) -> Env {
    let t = std::env::temp_dir().join(format!("bl-synconly-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&t);
    let (home, bin, repo) = (t.join("home"), t.join("bin"), t.join("repo"));
    for d in [&home, &bin, &repo] {
        std::fs::create_dir_all(d).unwrap();
    }
    symlink(tool_path("git"), bin.join("git")).unwrap();
    // The stub `gh` below shells out to `grep`, and `bl()` runs the binary
    // with `env_clear()` + `PATH=bin`, so the tools the stub itself needs
    // have to be in that same bin dir.
    symlink(tool_path("grep"), bin.join("grep")).unwrap();

    let ghlog = t.join("gh.log");
    // Stub gh: append the argv, then answer like the real thing.
    // `issue create` prints the new issue URL on stdout (that is how the
    // caller learns the number); everything else just exits 0.
    let stub = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {log}\n\
         if [ \"$1\" = issue ] && [ \"$2\" = create ]; then\n\
         \u{20} n=$(grep -c 'issue create' {log})\n\
         \u{20} echo \"https://github.com/o/r/issues/$((500 + n))\"\n\
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

    std::fs::create_dir_all(repo.join(".backlog")).unwrap();
    // Two pending tasks with no issue  -> 2 creates planned.
    std::fs::write(
        repo.join(".backlog/tasks.toml"),
        r#"[[task]]
id = "pend0001"
title = "pending one, unmirrored"
project = "/repo"
tags = []
status = "pending"
notes = ""
created_at = 1
updated_at = 1
weight = 0.0

[[task]]
id = "pend0002"
title = "pending two, unmirrored"
project = "/repo"
tags = []
status = "pending"
notes = ""
created_at = 2
updated_at = 2
weight = 0.0
"#,
    )
    .unwrap();
    // One done + one cancelled, each with an issue never confirmed closed
    // -> 2 closes planned.
    std::fs::write(
        repo.join(".backlog/tasks.done.toml"),
        r#"[[task]]
id = "done0001"
title = "done one, issue still open"
project = "/repo"
tags = []
status = "done"
notes = ""
created_at = 3
updated_at = 3
weight = 0.0
issue_number = 41
issue_url = "https://github.com/o/r/issues/41"

[[task]]
id = "canc0001"
title = "cancelled one, issue still open"
project = "/repo"
tags = []
status = "cancelled"
notes = ""
created_at = 4
updated_at = 4
weight = 0.0
issue_number = 42
issue_url = "https://github.com/o/r/issues/42"
"#,
    )
    .unwrap();

    Env {
        home,
        bin,
        repo,
        ghlog,
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

fn count(hay: &str, needle: &str) -> usize {
    hay.matches(needle).count()
}

fn store(p: &Path) -> (String, String) {
    (
        std::fs::read_to_string(p.join(".backlog/tasks.toml")).unwrap(),
        std::fs::read_to_string(p.join(".backlog/tasks.done.toml")).unwrap(),
    )
}

/// THE RULING. `--only close --apply` closes both stale issues and files
/// NOTHING. Dies if the filter is absent (2 creates would appear in the gh
/// log), if it is inverted, or if it drops the closes too (the control below
/// proves the closes are real work, not a no-op).
#[test]
fn only_close_closes_everything_and_creates_nothing() {
    let e = setup("close");
    let (c, o, er) = bl(&e, &["sync", "--only", "close", "--apply"]);
    let log = ghlog(&e);
    eprintln!("code={c}\nstdout={o}\nstderr={er}\nghlog={log}");
    assert_eq!(
        c, 0,
        "a fully-applied close reconciliation must exit 0: {er}"
    );
    assert_eq!(
        count(&log, "issue close"),
        2,
        "both stale issues must be closed; ghlog={log:?}"
    );
    assert_eq!(
        count(&log, "issue create"),
        0,
        "--only close must publish NO new issue; ghlog={log:?}"
    );
    // The closes were recorded, so a second sync is a no-op (idempotence).
    let (_, done) = store(&e.repo);
    assert_eq!(
        count(&done, "issue_closed_at"),
        2,
        "both confirmed closes must be stamped: {done}"
    );
    let (pending, _) = store(&e.repo);
    assert_eq!(
        count(&pending, "issue_number"),
        0,
        "the pending tasks must still be unmirrored: {pending}"
    );
}

/// The complement, so the test above cannot pass by filtering everything out:
/// `--only create` files both issues and closes nothing.
#[test]
fn only_create_files_everything_and_closes_nothing() {
    let e = setup("create");
    let (c, o, er) = bl(&e, &["sync", "--only", "create", "--apply"]);
    let log = ghlog(&e);
    eprintln!("code={c}\nstdout={o}\nstderr={er}\nghlog={log}");
    assert_eq!(
        c, 0,
        "a fully-applied create reconciliation must exit 0: {er}"
    );
    assert_eq!(
        count(&log, "issue create"),
        2,
        "both unmirrored pending tasks must be filed; ghlog={log:?}"
    );
    assert_eq!(
        count(&log, "issue close"),
        0,
        "--only create must close NOTHING; ghlog={log:?}"
    );
    let (_, done) = store(&e.repo);
    assert_eq!(
        count(&done, "issue_closed_at"),
        0,
        "no close may be recorded: {done}"
    );
}

/// BACKWARD COMPATIBILITY. No `--only` flag must behave exactly as before:
/// both arms run. Dies if the default is narrowed to one arm.
#[test]
fn default_still_reconciles_both_arms() {
    let e = setup("both");
    let (c, o, er) = bl(&e, &["sync", "--apply"]);
    let log = ghlog(&e);
    eprintln!("code={c}\nstdout={o}\nstderr={er}\nghlog={log}");
    assert_eq!(c, 0, "{er}");
    assert_eq!(count(&log, "issue create"), 2, "ghlog={log:?}");
    assert_eq!(count(&log, "issue close"), 2, "ghlog={log:?}");
}

/// The dry run must respect `--only` too: an operator checks the plan before
/// applying it, so a dry run that reports creates it would not perform is a
/// report about a different command than the one they are about to run.
#[test]
fn dry_run_plan_respects_only_and_performs_nothing() {
    let e = setup("dry");
    let (c, o, er) = bl(&e, &["sync", "--only", "close"]);
    eprintln!("code={c}\nstdout={o}\nstderr={er}");
    assert_eq!(c, 0, "{er}");
    assert!(
        o.contains("0 issue(s) to create") && o.contains("2 issue(s) to close"),
        "the dry-run plan must be the scoped plan: {o:?}"
    );
    assert_eq!(
        ghlog(&e),
        "",
        "a dry run must not invoke gh at all: {:?}",
        ghlog(&e)
    );
}
