// このファイルは丸ごと integration test なので unwrap/expect を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! An UNPARSEABLE claim registry is *cannot-determine*, not "nobody holds
//! anything".
//!
//! `claim::load` used to collapse both a missing file and a corrupt one into an
//! empty [`Registry`], and every consumer read that empty set as "no live
//! holder ⇒ safe to claim". A single corrupt `claims.json` therefore made every
//! other session's claims vanish AND let the next `state set --status running`
//! write a fresh registry over them — two workers on conflicting files at once
//! (CLAUDE.md §3: the empty set is the canonical fail-open shape).
//!
//! These tests drive the REAL binary (`condukt state set --status running`,
//! which is the production auto-claim path) against a sandboxed HOME, and pin
//! BOTH halves of the distinction:
//!
//! - corrupt registry  ⇒ the claim path must NOT succeed, and must not
//!   overwrite the bytes it could not read;
//! - absent registry   ⇒ a legitimately empty registry, the claim still
//!   succeeds (this half is what stops the fix from degrading into a blanket
//!   "always refuse").

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

struct Fixture {
    repo: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let pid = std::process::id();
        let mut base = std::env::temp_dir();
        base.push(format!("condukt-claim-corrupt-{pid}-{tag}"));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        run_git(&repo, &["init", "-q"]);
        run_git(&repo, &["config", "user.email", "t@t.t"]);
        run_git(&repo, &["config", "user.name", "t"]);
        Self { repo, home }
    }

    fn condukt(&self, args: &[&str]) -> Output {
        Command::new(bin())
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_SESSION_ID", "sess-test")
            .output()
            .expect("spawn condukt")
    }

    fn write_decomp(&self, name: &str, file: &str) -> PathBuf {
        let p = self.repo.join(name);
        let json = format!(
            r#"{{"goal":"touch {file}","tasks":[{{"id":"t1","title":"edit {file}","touched_files":["{file}"],"deps":[],"class":"parallel","done_criteria":"d"}}]}}"#
        );
        std::fs::write(&p, json).unwrap();
        p
    }

    fn init(&self, run: &str, decomp: &Path) {
        let out = self.condukt(&[
            "state",
            "init",
            "--run",
            run,
            "--file",
            decomp.to_str().unwrap(),
        ]);
        assert!(out.status.success(), "state init {run} failed: {out:?}");
    }

    fn set_running(&self, run: &str) -> Output {
        self.condukt(&[
            "state", "set", "--run", run, "--task", "t1", "--status", "running",
        ])
    }

    /// Locate the on-disk `claims.json` the binary writes under the sandboxed
    /// HOME (`<base>/state/<project-key>/claims.json`), without duplicating the
    /// private path derivation.
    fn find_claims_json(&self) -> Option<PathBuf> {
        find_named(&self.home, "claims.json")
    }
}

fn find_named(dir: &Path, name: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Some(found) = find_named(&p, name) {
                return Some(found);
            }
        } else if p.file_name().and_then(|n| n.to_str()) == Some(name) {
            return Some(p);
        }
    }
    None
}

fn run_git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git")
        .status
        .success();
    assert!(ok, "git {args:?} failed");
}

/// Half (a): an UNPARSEABLE registry must not grant a claim.
///
/// runB legitimately holds `src/other.rs`. The registry is then corrupted (as a
/// torn write / manual edit / disk error would). runA asking to run must NOT be
/// told "go ahead": the registry cannot be read, so whether anyone holds
/// `src/shared.rs` is unknown. It must also not clobber the bytes it failed to
/// parse — overwriting them destroys runB's claim permanently.
#[test]
fn corrupt_registry_refuses_to_grant_a_claim_and_does_not_clobber_it() {
    let fx = Fixture::new("corrupt");
    let dec_a = fx.write_decomp("decA.json", "src/shared.rs");
    let dec_b = fx.write_decomp("decB.json", "src/other.rs");
    fx.init("runA", &dec_a);
    fx.init("runB", &dec_b);

    // runB claims a disjoint file, so the registry file genuinely exists.
    let b = fx.set_running("runB");
    assert!(b.status.success(), "precondition: runB must claim: {b:?}");
    let path = fx
        .find_claims_json()
        .expect("precondition: claims.json must exist after a successful claim");

    // Corrupt it.
    let corrupt: &[u8] = b"{ this is not valid json ]]";
    std::fs::write(&path, corrupt).unwrap();

    let a = fx.set_running("runA");
    assert!(
        !a.status.success(),
        "an unreadable claim registry must NOT grant a claim (cannot-determine \
         is not 'nobody holds anything'); got success: stdout={} stderr={}",
        String::from_utf8_lossy(&a.stdout),
        String::from_utf8_lossy(&a.stderr)
    );

    let after = std::fs::read(&path).unwrap();
    assert_eq!(
        after, corrupt,
        "the claim path overwrote a registry it could not parse — runB's claim \
         is now gone (this is the mass double-claim window)"
    );
}

/// Half (b): a MISSING registry is a legitimately empty one — nobody has
/// claimed yet — so the claim still succeeds. Without this the fix could
/// degrade into a blanket refusal, which would break every first claim in a
/// fresh project.
#[test]
fn absent_registry_is_legitimately_empty_and_a_claim_still_succeeds() {
    let fx = Fixture::new("absent");
    let dec_a = fx.write_decomp("decA.json", "src/shared.rs");
    fx.init("runA", &dec_a);

    assert!(
        fx.find_claims_json().is_none(),
        "precondition: no registry may exist yet"
    );

    let a = fx.set_running("runA");
    assert!(
        a.status.success(),
        "an ABSENT registry means nobody has claimed yet — the claim must \
         succeed: stdout={} stderr={}",
        String::from_utf8_lossy(&a.stdout),
        String::from_utf8_lossy(&a.stderr)
    );

    let path = fx
        .find_claims_json()
        .expect("a successful claim must persist a registry");
    let txt = std::fs::read_to_string(&path).unwrap();
    assert!(
        txt.contains("src/shared.rs") && txt.contains("runA"),
        "the persisted registry must record runA holding src/shared.rs, got: {txt}"
    );
}
