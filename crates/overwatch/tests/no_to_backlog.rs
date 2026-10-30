#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `overwatch review-queue --to-backlog` was removed entirely (user ruling
//! 2026-10-15): no CLI flag, no bridge, no bridge idempotency ledgers.
//! Findings are filed with `backlog add --repro-test`; overwatch never calls
//! `backlog add`. These tests drive the built binary with an isolated HOME and
//! a fake `backlog` first on PATH that records any invocation.
#![cfg(unix)]
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_overwatch")
}

struct Fx {
    root: PathBuf,
    home: PathBuf,
    project: PathBuf,
    bindir: PathBuf,
    log: PathBuf,
}

fn fixture(tag: &str) -> Fx {
    let root =
        std::env::temp_dir().join(format!("overwatch-no-bridge-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let home = root.join("home");
    let project = root.join("project");
    let bindir = root.join("bin");
    let log = root.join("backlog-called.log");
    for d in [&home, &project, &bindir] {
        fs::create_dir_all(d).unwrap();
    }
    let script = bindir.join("backlog");
    fs::write(
        &script,
        "#!/bin/sh\necho \"$@\" >> \"$FAKE_BACKLOG_LOG\"\nexit 0\n",
    )
    .unwrap();
    let mut p = fs::metadata(&script).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&script, p).unwrap();
    Fx {
        root,
        home,
        project,
        bindir,
        log,
    }
}

fn run(fx: &Fx, args: &[&str]) -> Output {
    let path = format!(
        "{}:{}",
        fx.bindir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    harness_core::git_env::scrub(&mut Command::new(bin()))
        .args(args)
        .current_dir(&fx.project)
        .env("HOME", &fx.home)
        .env("PATH", path)
        .env("FAKE_BACKLOG_LOG", &fx.log)
        .output()
        .expect("spawn overwatch")
}

fn seed_finding(fx: &Fx) {
    // Library resolves storage root from process HOME; seed under the isolated one.
    std::env::set_var("HOME", &fx.home);
    overwatch::store::append_review_finding(
        &fx.project,
        &overwatch::review_finding::ReviewFinding::new(
            "F-1".into(),
            "reviewgate".into(),
            Some("high".into()),
            "unchecked unwrap in foo.rs".into(),
            Some("src/foo.rs".into()),
            None,
            100,
        ),
    )
    .unwrap();
}

fn find_named(dir: &Path, names: &[&str], out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            find_named(&p, names, out);
        } else if p
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| names.contains(&n))
        {
            out.push(p);
        }
    }
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

#[test]
fn to_backlog_flag_is_rejected_and_backlog_never_invoked() {
    let fx = fixture("flag");
    seed_finding(&fx);
    let out = run(&fx, &["review-queue", "--to-backlog"]);
    let t = text(&out);
    assert!(
        !out.status.success(),
        "--to-backlog must be rejected, got success: {t}"
    );
    assert!(
        t.contains("unexpected argument") || t.contains("unrecognized"),
        "expected a clap unknown-argument error, got: {t}"
    );
    assert!(
        !fx.log.exists(),
        "backlog must not be invoked: {:?}",
        fs::read_to_string(&fx.log)
    );
    let _ = fs::remove_dir_all(&fx.root);
}

#[test]
fn review_queue_help_does_not_mention_to_backlog() {
    let fx = fixture("rqhelp");
    let out = run(&fx, &["review-queue", "--help"]);
    let t = text(&out);
    assert!(out.status.success(), "--help should work: {t}");
    assert!(
        !t.contains("to-backlog"),
        "help still mentions to-backlog:\n{t}"
    );
    let _ = fs::remove_dir_all(&fx.root);
}

#[test]
fn plain_review_queue_creates_no_bridge_ledger_and_calls_no_backlog() {
    let fx = fixture("plain");
    seed_finding(&fx);
    let out = run(&fx, &["review-queue"]);
    assert!(
        out.status.success(),
        "plain review-queue should succeed: {}",
        text(&out)
    );
    let mut found = Vec::new();
    find_named(
        &fx.home,
        &["bridged_findings.jsonl", "bridged_entries.jsonl"],
        &mut found,
    );
    assert!(found.is_empty(), "bridge ledgers must not exist: {found:?}");
    assert!(!fx.log.exists(), "backlog must not be invoked");
    let _ = fs::remove_dir_all(&fx.root);
}

#[test]
fn no_help_surface_mentions_to_backlog() {
    let fx = fixture("allhelp");
    let top = text(&run(&fx, &["--help"]));
    assert!(
        !top.contains("to-backlog"),
        "top-level help mentions to-backlog:\n{top}"
    );
    // Enumerate subcommands from top-level help and check each one's help.
    let mut subs = Vec::new();
    let mut in_cmds = false;
    for line in top.lines() {
        if line.starts_with("Commands:") {
            in_cmds = true;
            continue;
        }
        if in_cmds {
            if !line.starts_with("  ") {
                break;
            }
            if let Some(name) = line.split_whitespace().next() {
                if name != "help" {
                    subs.push(name.to_string());
                }
            }
        }
    }
    assert!(
        !subs.is_empty(),
        "could not enumerate subcommands from help:\n{top}"
    );
    for s in &subs {
        let h = text(&run(&fx, &[s, "--help"]));
        assert!(
            !h.contains("to-backlog"),
            "`{s} --help` mentions to-backlog:\n{h}"
        );
    }
    let _ = fs::remove_dir_all(&fx.root);
}
