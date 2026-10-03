// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Independent black-box tests for `hypothesis draft` (backlog 40408d7f).
//!
//! The unit tests in `src/draft.rs` were written by the implementing agent
//! (its module says so). These are written by a different agent, against the
//! binary only, for the fail-closed direction the module promises: while any
//! rigor point is open, NO hypothesis record is written. The control
//! (`fully_answered_draft_writes_exactly_one_record`) keeps the first test from
//! passing vacuously: the same fixture really can write a record.
//!
//! Not covered here: `Store::add_draft` accepting only a `ScopeDeclaration` is a
//! type-level property (no public constructor), not observable from the CLI.

use std::path::{Path, PathBuf};
use std::process::Command;

fn home(tag: &str) -> PathBuf {
    let h = std::env::temp_dir().join(format!(
        "hyp-draft-indep-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&h).unwrap();
    h
}

fn draft(home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_hypothesis"))
        .arg("draft")
        .args(args)
        .env("HOME", home)
        .env_remove("HYPOTHESIS_DISABLE")
        .current_dir(home)
        .output()
        .expect("spawn hypothesis")
}

fn store_file(home: &Path) -> PathBuf {
    home.join(".hypothesis").join("hypotheses.toml")
}

fn record_count(home: &Path) -> usize {
    match std::fs::read_to_string(store_file(home)) {
        Ok(s) => s.matches("[[hypotheses]]").count(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
        Err(e) => panic!("cannot read the hypothesis store: {e}"),
    }
}

const FULL: [&str; 9] = [
    "--success",
    "act >= 0.4",
    "--kill",
    "act <= 0.2",
    "--goal",
    "g",
    "--write-path",
    "a.rs",
    "--no-read-paths",
];

#[test]
fn fully_answered_draft_writes_exactly_one_record() {
    let h = home("full");
    let out = draft(&h, &[&["ship x"][..], &FULL[..]].concat());
    assert!(out.status.success(), "{out:?}");
    assert!(
        store_file(&h).exists(),
        "control: a fully answered draft must reach the store: {out:?}"
    );
    assert_eq!(record_count(&h), 1, "control: exactly one record");
}

#[test]
fn each_single_missing_answer_writes_nothing() {
    // Drop one answer at a time (flag + value, or the bare --no-read-paths).
    let drops: [&[usize]; 5] = [&[0, 1], &[2, 3], &[4, 5], &[6, 7], &[8]];
    for drop in drops {
        let h = home("partial");
        let args: Vec<&str> = std::iter::once("ship x")
            .chain(
                FULL.iter()
                    .enumerate()
                    .filter(|(i, _)| !drop.contains(i))
                    .map(|(_, a)| *a),
            )
            .collect();
        let out = draft(&h, &args);
        assert!(
            !store_file(&h).exists(),
            "an open rigor point must leave the store untouched, but {} exists \
             after `draft {args:?}`: {out:?}",
            store_file(&h).display()
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains('['),
            "the open item must be named on stdout for `draft {args:?}`: {stdout}"
        );
    }
}

#[test]
fn answering_write_surface_with_nothing_writes_nothing() {
    let h = home("nowrite");
    let out = draft(
        &h,
        &[
            "ship x",
            "--success",
            "act >= 0.4",
            "--kill",
            "act <= 0.2",
            "--goal",
            "g",
            "--no-write-paths",
            "--no-read-paths",
        ],
    );
    assert!(
        !store_file(&h).exists(),
        "`--no-write-paths` is a refusal to declare a write surface; nothing may \
         be recorded: {out:?}"
    );
}
