#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repros for the proof-content contract (user ruling relayed 2026-11-17).
//! A proof written by `tdd red` / `tdd green` must contain: (a) the command,
//! (b) tree identity = commit sha + hash of the uncommitted diff, (c) the exit
//! code, (d) per-test results by name (pass/fail/ignored) and counts, (e) the
//! output tail.
//!
//! Status at HEAD (observed from the proof JSON of this file's runs):
//!   (a) OK  `cmd` in both proofs        (c) OK  `exit_code` in both proofs
//!   (e) RED for green only (red has `output_tail`): `green_proof_carries_output_tail_like_red`
//!   (b) RED `proofs_carry_tree_identity`
//!   (d) RED `proofs_carry_per_test_results_and_counts`
//! Origin of (e): condukt run-20261116-020031-33040: green proof could not
//! say how many tests passed, red proof recorded "13 failed".
//!
//! Reading (A, asserted): green is symmetric with red and carries the same
//! `output_tail` field with the runner's captured output. Reading (B,
//! intended): green deliberately omits it; then this test must be dropped and
//! a human must say where the pass count is meant to be found.
//!
//! Shape: temp git repo; command prints a count then fails until impl.txt
//! exists (RED), then passes (GREEN). Controls: both steps succeed, and the
//! RED proof's output_tail contains the printed count.
//!
//! DOES NOT PROVE: the tail's truncation (output_tail_lines); real cargo test
//! output; the field's name if the fix chooses a different one (this test
//! pins `output_tail`, "the same field as red").
//!
//! Written by an independent test author, not the implementer.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

struct Dir(PathBuf);
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tdd(home: &Path, cwd: &Path, args: &[&str]) -> Output {
    harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_tdd")))
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .stdin(Stdio::null())
        .output()
        .expect("tdd runs")
}

fn git(cwd: &Path, args: &[&str]) {
    let o = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git runs");
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

fn s(o: &Output) -> String {
    format!(
        "exit={:?}\nstdout: {}\nstderr: {}",
        o.status.code(),
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn repo(tag: &str) -> (Dir, PathBuf, PathBuf) {
    let d = Dir(std::env::temp_dir().join(format!("tdd-{tag}-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&d.0);
    let home = d.0.join("home");
    let work = d.0.join("work");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    git(&work, &["init", "-q", "-b", "main"]);
    git(&work, &["config", "user.email", "t@example.invalid"]);
    git(&work, &["config", "user.name", "t"]);
    std::fs::write(work.join("a.txt"), "a\n").unwrap();
    git(&work, &["add", "a.txt"]);
    git(&work, &["commit", "-q", "--no-verify", "-m", "init"]);
    (d, home, work)
}

fn proof(work: &Path, kind: &str) -> serde_json::Value {
    let p = work.join(".tdd").join(format!("t1.{kind}.json"));
    serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap()
}

fn strip_tail(mut v: serde_json::Value) -> serde_json::Value {
    v.as_object_mut().unwrap().remove("output_tail");
    v
}

fn numbers(v: &serde_json::Value, out: &mut Vec<i64>) {
    match v {
        serde_json::Value::Number(n) => out.extend(n.as_i64()),
        serde_json::Value::Array(a) => a.iter().for_each(|x| numbers(x, out)),
        serde_json::Value::Object(o) => o.values().for_each(|x| numbers(x, out)),
        _ => {}
    }
}

fn hexish(v: &serde_json::Value, out: &mut Vec<String>) {
    match v {
        serde_json::Value::String(s)
            if s.len() >= 32 && s.chars().all(|c| c.is_ascii_hexdigit()) =>
        {
            out.push(s.clone())
        }
        serde_json::Value::Array(a) => a.iter().for_each(|x| hexish(x, out)),
        serde_json::Value::Object(o) => o.values().for_each(|x| hexish(x, out)),
        _ => {}
    }
}

// test-weakening-justified: new RED for an unfixed defect (2026-11-17).
#[ignore = "RED: 2026-11-17 green proof records no output_tail (red does)"]
#[test]
fn green_proof_carries_output_tail_like_red() {
    let d = Dir(std::env::temp_dir().join(format!("tdd-green-tail-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&d.0);
    let home = d.0.join("home");
    let work = d.0.join("work");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    git(&work, &["init", "-q", "-b", "main"]);
    git(&work, &["config", "user.email", "t@example.invalid"]);
    git(&work, &["config", "user.name", "t"]);
    std::fs::write(work.join("a.txt"), "a\n").unwrap();
    git(&work, &["add", "a.txt"]);
    git(&work, &["commit", "-q", "--no-verify", "-m", "init"]);

    let cmd = "echo 'test result: 13 counted'; test -f impl.txt";
    let red = tdd(&home, &work, &["red", "--task", "t1", "--cmd", cmd]);
    assert_eq!(
        red.status.code(),
        Some(0),
        "control: RED recorded: {}",
        s(&red)
    );
    std::fs::write(work.join("impl.txt"), "impl\n").unwrap();
    let green = tdd(&home, &work, &["green", "--task", "t1", "--cmd", cmd]);
    assert_eq!(
        green.status.code(),
        Some(0),
        "control: GREEN recorded: {}",
        s(&green)
    );

    let read = |kind: &str| -> serde_json::Value {
        let p = work.join(".tdd").join(format!("t1.{kind}.json"));
        serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap()
    };
    let red_json = read("red");
    let green_json = read("green");
    assert!(
        red_json["output_tail"]
            .as_str()
            .is_some_and(|t| t.contains("13 counted")),
        "control: RED proof records the output: {red_json}"
    );
    assert!(
        green_json["output_tail"]
            .as_str()
            .is_some_and(|t| t.contains("13 counted")),
        "GREEN proof has no output_tail containing the run's output, so the pass count is unrecoverable.\ngreen proof: {green_json}"
    );
}

/// (b) tree identity: commit sha + hash of the uncommitted diff, in both proofs.
// test-weakening-justified: new RED for an unfixed defect (2026-11-17).
#[ignore = "RED: 2026-11-17 tdd proofs carry no tree identity (commit sha + uncommitted-diff hash)"]
#[test]
fn proofs_carry_tree_identity() {
    let (_d, home, work) = repo("treeid");
    let head = {
        let o = harness_core::git_env::scrub(&mut Command::new("git"))
            .args(["rev-parse", "HEAD"])
            .current_dir(&work)
            .output()
            .unwrap();
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    };
    // Uncommitted tracked edit present at RED, a different one at GREEN.
    std::fs::write(work.join("a.txt"), "dirty-red\n").unwrap();
    let cmd = "test -f impl.txt";
    let red = tdd(&home, &work, &["red", "--task", "t1", "--cmd", cmd]);
    assert_eq!(
        red.status.code(),
        Some(0),
        "control: RED recorded: {}",
        s(&red)
    );
    std::fs::write(work.join("impl.txt"), "impl\n").unwrap();
    std::fs::write(work.join("a.txt"), "dirty-green\n").unwrap();
    let green = tdd(&home, &work, &["green", "--task", "t1", "--cmd", cmd]);
    assert_eq!(
        green.status.code(),
        Some(0),
        "control: GREEN: {}",
        s(&green)
    );
    let (r, g) = (proof(&work, "red"), proof(&work, "green"));
    for (name, p) in [("red", &r), ("green", &g)] {
        assert!(
            p.to_string().contains(&head),
            "{name} proof does not record the commit sha {head}: {p}"
        );
    }
    let (mut rh, mut gh) = (vec![], vec![]);
    hexish(&r, &mut rh);
    hexish(&g, &mut gh);
    rh.retain(|h| *h != head);
    gh.retain(|h| *h != head);
    assert!(
        !rh.is_empty() && !gh.is_empty() && rh != gh,
        "proofs must each carry a hash of the uncommitted diff (differing here: a.txt was \
         edited differently at RED and GREEN); red={rh:?} green={gh:?}\nred: {r}\ngreen: {g}"
    );
}

/// (d) per-test results by name (pass/fail/ignored) and counts. Test names and
/// counts are looked for OUTSIDE `output_tail`, which holds only raw text.
// test-weakening-justified: new RED for an unfixed defect (2026-11-17).
#[ignore = "RED: 2026-11-17 tdd proofs carry no per-test results or counts"]
#[test]
fn proofs_carry_per_test_results_and_counts() {
    let (_d, home, work) = repo("pertest");
    // libtest-style lines. RED: 2 ok, 3 FAILED, 4 ignored. GREEN: 5 ok, 0 failed, 4 ignored.
    let script = "\
if [ -f impl.txt ]; then\n\
  for i in 1 2 3 4 5; do echo \"test m::p$i ... ok\"; done\n\
  for i in 1 2 3 4; do echo \"test m::i$i ... ignored\"; done\n\
  echo 'test result: ok. 5 passed; 0 failed; 4 ignored'; exit 0\n\
fi\n\
for i in 1 2; do echo \"test m::p$i ... ok\"; done\n\
for i in 1 2 3; do echo \"test m::f$i ... FAILED\"; done\n\
for i in 1 2 3 4; do echo \"test m::i$i ... ignored\"; done\n\
echo 'test result: FAILED. 2 passed; 3 failed; 4 ignored'; exit 1\n";
    std::fs::write(work.join("t.sh"), script.replace("\\n", "\n")).unwrap();
    let cmd = "sh t.sh";
    let red = tdd(&home, &work, &["red", "--task", "t1", "--cmd", cmd]);
    assert_eq!(
        red.status.code(),
        Some(0),
        "control: RED recorded: {}",
        s(&red)
    );
    std::fs::write(work.join("impl.txt"), "impl\n").unwrap();
    let green = tdd(&home, &work, &["green", "--task", "t1", "--cmd", cmd]);
    assert_eq!(
        green.status.code(),
        Some(0),
        "control: GREEN: {}",
        s(&green)
    );
    let (r, g) = (
        strip_tail(proof(&work, "red")),
        strip_tail(proof(&work, "green")),
    );
    let (rt, gt) = (r.to_string(), g.to_string());
    for name in ["m::p1", "m::f3", "m::i2"] {
        assert!(
            rt.contains(name),
            "red proof lacks per-test entry {name} outside output_tail: {r}"
        );
    }
    for name in ["m::p5", "m::i4"] {
        assert!(
            gt.contains(name),
            "green proof lacks per-test entry {name} outside output_tail: {g}"
        );
    }
    let (mut rn, mut gn) = (vec![], vec![]);
    numbers(&r, &mut rn);
    numbers(&g, &mut gn);
    for n in [2, 3, 4] {
        assert!(
            rn.contains(&n),
            "red proof lacks count {n} (2 passed/3 failed/4 ignored): {r}"
        );
    }
    for n in [5, 4] {
        assert!(
            gn.contains(&n),
            "green proof lacks count {n} (5 passed/4 ignored): {g}"
        );
    }
}
