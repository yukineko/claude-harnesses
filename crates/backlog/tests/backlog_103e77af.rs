#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog 103e77af: `add`'s belt 2 (`confirm_added_id_is_on_disk`, the
//! post-save reload-and-confirm) had kill rate 0 — deleting the call left all
//! tests green, because inside the lock no test could make "save reported Ok,
//! but a fresh load does not contain the id" happen.
//!
//! This test produces exactly that state deterministically without touching
//! production code: a DYLD interposer (macOS) turns `rename(2)` onto
//! `.backlog/tasks.toml` into a successful no-op, so `save` returns Ok while
//! the file on disk never gains the new row. Belt 2 must then refuse; without
//! belt 2, `add` prints `added:` and exits 0 about a task nobody can read.
//!
//! The control proves the interposer is inert unless armed, so the RED of the
//! main assertion cannot come from a broken fixture.
//!
//! Written by an independent auditor, not an implementer.

#![cfg(target_os = "macos")]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const SHIM_C: &str = r#"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
static int bh_rename(const char *from, const char *to) {
    const char *suf = "/.backlog/tasks.toml";
    size_t n = strlen(to), s = strlen(suf);
    if (getenv("BACKLOG_103E77AF_BLACKHOLE") && n >= s && strcmp(to + n - s, suf) == 0) {
        unlink(from);
        return 0;
    }
    return rename(from, to);
}
__attribute__((used)) static struct { const void *repl; const void *orig; } interposers[]
    __attribute__((section("__DATA,__interpose"))) = { { (const void *)bh_rename, (const void *)rename } };
"#;

fn unique(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "backlog-103e77af-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(&dir).unwrap()
}

fn build_shim(dir: &Path) -> PathBuf {
    let src = dir.join("bh.c");
    let lib = dir.join("bh.dylib");
    std::fs::write(&src, SHIM_C).unwrap();
    let out = Command::new("cc")
        .args(["-dynamiclib", "-o"])
        .arg(&lib)
        .arg(&src)
        .output()
        .expect("cc runs");
    assert!(
        out.status.success(),
        "fixture is void: cc could not build the interposer: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    lib
}

struct Fx {
    home: PathBuf,
    repo: PathBuf,
    lib: PathBuf,
}

fn fx(tag: &str) -> Fx {
    let root = unique(tag);
    let (home, repo) = (root.join("home"), root.join("repo"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    common::linked_checkout(&repo);
    let lib = build_shim(&root);
    Fx { home, repo, lib }
}

fn add(f: &Fx, title: &str, armed: bool) -> (i32, String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_backlog"));
    cmd.env("PATH", common::path_with_condukt_shim());
    cmd.args([
        "add",
        "--title",
        title,
        "--project",
        f.repo.to_str().unwrap(),
    ])
    .env("HOME", &f.home)
    .env("DYLD_INSERT_LIBRARIES", &f.lib)
    .current_dir(&f.repo)
    .stdin(Stdio::null());
    if armed {
        cmd.env("BACKLOG_103E77AF_BLACKHOLE", "1");
    }
    let out = cmd.output().expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn control_unarmed_interposer_is_inert() {
    let f = fx("control");
    let (rc, out, err) = add(&f, "control task", false);
    assert_eq!(rc, 0, "unarmed add must succeed: out={out} err={err}");
    assert!(out.contains("added: "), "out={out:?}");
    let store = std::fs::read_to_string(f.repo.join(".backlog/tasks.toml")).unwrap();
    assert!(store.contains("control task"), "store={store}");
}

#[test]
fn add_refuses_when_a_fresh_load_does_not_contain_the_saved_id() {
    let f = fx("armed");
    let (rc, out, err) = add(&f, "black-holed task", true);
    let on_disk = std::fs::read_to_string(f.repo.join(".backlog/tasks.toml")).unwrap_or_default();
    assert!(
        !on_disk.contains("black-holed task"),
        "precondition: the armed interposer must keep the row off disk; store={on_disk}"
    );
    assert_ne!(
        rc, 0,
        "add reported success about a task a fresh load cannot see: out={out:?} err={err:?}"
    );
    assert!(
        !out.contains("added: "),
        "add printed `added:` for a row that is not on disk: out={out:?}"
    );
    assert!(
        err.contains("does not contain it"),
        "the refusal must be belt 2's (post-save reload), not some other error: err={err:?}"
    );
}
