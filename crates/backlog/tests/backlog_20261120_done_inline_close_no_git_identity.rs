#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Backlog 410e9514: `done_closes_issue_inline.rs` was observed failing on a
//! Linux host with "Author identity unknown" — `add` lands the row with
//! `git commit-tree`, and the fixture's `bl()` runs the binary with
//! `env_clear()` + an empty `HOME` and no `GIT_AUTHOR_*` / `GIT_COMMITTER_*`.
//!
//! On a macOS host git AUTO-DETECTS an identity (login name @ `<host>.local`),
//! so the original test passes there and says nothing about the claim. This
//! file runs the same add → done-inline-close scenario with the binary in the
//! same env shape as `bl()` (env_clear, PATH = stub bin dir, isolated HOME),
//! plus an empty XDG_CONFIG_HOME and GIT_CONFIG_NOSYSTEM=1, in two variants:
//!
//! * `auto_detected_identity`: nothing else; git may auto-detect an identity
//!   from the host (the condition the original test runs under here).
//! * `no_detectable_identity`: the isolated global config also sets
//!   `user.useConfigOnly = true`, so git cannot auto-detect either — the
//!   host-independent model of a machine whose auto-detection yields nothing
//!   (the Linux observation). A probe asserts `git var GIT_AUTHOR_IDENT` really
//!   fails under that env, so the variant is not vacuous.
//!
//! DOES NOT PROVE: which fix is right (fixture identity env vs. the binary
//! supplying an identity for its own bookkeeping commits); behaviour on hosts
//! whose auto-detection succeeds with a different identity.
mod common;

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

fn write_exec(p: &Path, body: &str) {
    std::fs::write(p, body).unwrap();
    std::fs::set_permissions(
        p,
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
    )
    .unwrap();
}

struct Env {
    home: PathBuf,
    xdg: PathBuf,
    bin: PathBuf,
    repo: PathBuf,
    ghlog: PathBuf,
}

fn setup(tag: &str, use_config_only: bool) -> Env {
    let t = std::env::temp_dir().join(format!("bl-noident-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&t);
    let (home, xdg, bin, repo) = (t.join("home"), t.join("xdg"), t.join("bin"), t.join("repo"));
    for d in [&home, &xdg, &bin, &repo] {
        std::fs::create_dir_all(d).unwrap();
    }
    symlink(tool_path("git"), bin.join("git")).unwrap();
    symlink(tool_path("grep"), bin.join("grep")).unwrap();
    symlink("/bin/bash", bin.join("bash")).unwrap();
    write_exec(
        &bin.join("condukt"),
        "#!/bin/sh\necho '{\"claimed\":false}'\nexit 1\n",
    );
    let _ = Command::new(bin.join("condukt"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    let ghlog = t.join("gh.log");
    write_exec(
        &bin.join("gh"),
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {log}\n\
             if [ \"$1\" = issue ] && [ \"$2\" = create ]; then\n\
             \u{20} n=$(grep -c 'issue create' {log})\n\
             \u{20} echo \"https://github.com/o/r/issues/$((700 + n))\"\n\
             fi\nexit 0\n",
            log = ghlog.display()
        ),
    );

    // Fixture repo construction uses explicit identities (as the original
    // fixture does); only the binary under test runs identity-less.
    common::linked_checkout(&repo);
    assert!(harness_core::git_env::scrub(&mut Command::new("git"))
        .args(["remote", "add", "origin", "https://github.com/o/r.git"])
        .current_dir(&repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .status()
        .unwrap()
        .success());
    common::local_push_origin(&common::linked_primary(&repo));
    common::commit_repro(&repo);

    if use_config_only {
        std::fs::write(home.join(".gitconfig"), "[user]\n\tuseConfigOnly = true\n").unwrap();
    }
    Env {
        home,
        xdg,
        bin,
        repo,
        ghlog,
    }
}

/// The binary's environment: `bl()`'s shape plus the isolation knobs.
fn identityless(cmd: &mut Command, e: &Env) {
    cmd.current_dir(&e.repo)
        .env_clear()
        .env("HOME", &e.home)
        .env("XDG_CONFIG_HOME", &e.xdg)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("PATH", &e.bin);
}

fn bl(e: &Env, args: &[&str]) -> (i32, String, String) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_backlog"));
    c.args(args);
    identityless(&mut c, e);
    let o = c.output().unwrap();
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

fn author_ident_probe(e: &Env) -> (bool, String) {
    let mut c = Command::new(e.bin.join("git"));
    c.args(["var", "GIT_AUTHOR_IDENT"]);
    identityless(&mut c, e);
    let o = c.output().unwrap();
    (
        o.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        ),
    )
}

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

fn add_then_done_closes_inline(e: &Env) {
    let (c, o, er) = bl(
        e,
        &[
            "add",
            "--title",
            "inline close me",
            "--project",
            ".",
            "--repro-test",
            common::COMMITTED_REPRO,
        ],
    );
    assert_eq!(c, 0, "add failed: stdout={o:?} stderr={er:?}");
    let id = o
        .lines()
        .find_map(|l| l.strip_prefix("added: "))
        .expect("add must print the new id")
        .trim()
        .to_string();
    seed_duplicate_target(&e.repo.join(".backlog/tasks.done.toml"));
    let (c, o, er) = bl(e, &["done", &id, "--duplicate-of", DUP_TARGET]);
    assert_eq!(c, 0, "done failed: stdout={o:?} stderr={er:?}");
    let log = std::fs::read_to_string(&e.ghlog).unwrap_or_default();
    assert!(
        log.contains("issue close 701 --reason completed"),
        "done must close the task's own issue inline; ghlog={log:?}"
    );
}

#[test]
fn auto_detected_identity_add_then_done_closes_inline() {
    let e = setup("auto", false);
    add_then_done_closes_inline(&e);
}

#[test]
#[ignore = "backlog 410e9514: RED observed 2026-11-20 on macOS: with no detectable git author identity, add's commit-tree exits 128 'Author identity unknown' and add fails before done's inline close is reached — remove when fixed"]
fn no_detectable_identity_add_then_done_closes_inline() {
    let e = setup("none", true);
    let (ok, out) = author_ident_probe(&e);
    assert!(
        !ok,
        "fixture is vacuous: git still found an author identity: {out:?}"
    );
    add_then_done_closes_inline(&e);
}
