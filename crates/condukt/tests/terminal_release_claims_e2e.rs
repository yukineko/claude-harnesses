// このファイルは丸ごと integration test なので unwrap/expect を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Every path that drives a task to a TERMINAL status must hand back that
//! task's file claims — not just the one path that currently does.
//!
//! `claim::release_files` has exactly ONE automatic caller: the `state set`
//! handler in `main.rs`, gated on `Verified | Failed | Cancelled`. Every other
//! way a task reaches a terminal status bypasses it:
//!
//! - `state reconcile` → `state::reconcile_run` writes
//!   `Verified | Cancelled | Discarded` and never releases;
//! - `state cancel` writes `Status::Cancelled` directly and never releases;
//! - `worktree discard` → `state::discard_experiment` writes
//!   `Status::Discarded` and never releases;
//! - and the `state set` gate itself omits `Discarded`, even though
//!   `Discarded` is terminal everywhere else (`state::gate_reasons`,
//!   `state::reconcile_run`, `wt_reconcile`).
//!
//! On top of that, the one caller that DOES release is fed by `task_files`,
//! which returns `Vec::new()` on ANY failure — decomposition unreadable,
//! unparseable, or task id absent — and the caller guards `if
//! !files.is_empty()`. So "I could not find out which files to release" and
//! "there are no files to release" are the same observation, and the release is
//! skipped ENTIRELY and SILENTLY. That is the empty-set fail-open CLAUDE.md §3
//! names verbatim ("エラー時に空の集合を返さない。空集合は下流で「検査対象なし ＝
//! 合格」と読まれる").
//!
//! Observed consequence (backlog `06eb8aa3`): a verifier run held 13 file
//! claims including `.claude-plugin/marketplace.json`, `Cargo.toml` and
//! `plugin.json` — files EVERY plugin task must touch because of version
//! lockstep — while `state reconcile` reported 3/3 verified and `state gate`
//! reported PASS. All 13 had to be released by hand.
//!
//! ## Deliberate NON-gap, pinned here as a regression guard
//!
//! `Status::Done` is NOT terminal — it awaits verification — so it must NOT
//! release. `done_status_keeps_its_claims` pins that and is GREEN before the
//! fix (see also `tests/heartbeat_err_surfaced.rs`, which uses `--status done`
//! precisely because it does not trigger a release).
//!
//! ## An unresolved contradiction the implementer MUST settle, not paper over
//!
//! `tests/heartbeat_err_surfaced.rs` pins, for `state set`, that a FAILING
//! claim-upkeep step is loud on stderr but leaves the exit code at 0, because
//! the durable state write already succeeded. Backlog `06eb8aa3`'s
//! done_criteria says instead that "a release that CANNOT be completed must NOT
//! be silent: it must NAME what stayed held and exit NON-ZERO". Those two
//! cannot both hold for `state set`.
//!
//! The two assertions are therefore split into two separate tests below —
//! `unreadable_decomposition_terminal_set_is_not_silent` (loudness only, which
//! contradicts nothing) and
//! `unreadable_decomposition_terminal_set_exits_non_zero` (the exit-code half,
//! which is the half in tension). Resolving that tension is a DECISION, not an
//! implementation detail; do not quietly delete either test.

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
    /// Per-test sandbox: pid (unique per test binary process, so two concurrent
    /// `cargo test` runs cannot collide) + a per-test tag + the process start
    /// nanos, so a stale directory from a crashed run is never reused either.
    fn new(tag: &str) -> Self {
        let pid = std::process::id();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let mut base = std::env::temp_dir();
        base.push(format!("condukt-term-release-{pid}-{tag}-{nonce}"));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        run_git(&repo, &["init", "-q", "-b", "main"]);
        run_git(&repo, &["config", "user.email", "t@t.t"]);
        run_git(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("base.txt"), "base\n").unwrap();
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-q", "-m", "init"]);
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

    /// A decomposition with one entry per `(task_id, touched_file)` pair.
    fn write_decomp(&self, name: &str, tasks: &[(&str, &str)]) -> PathBuf {
        let p = self.repo.join(name);
        let entries: Vec<String> = tasks
            .iter()
            .map(|(id, file)| {
                format!(
                    r#"{{"id":"{id}","title":"edit {file}","touched_files":["{file}"],"deps":[],"class":"parallel","done_criteria":"d"}}"#
                )
            })
            .collect();
        let json = format!(r#"{{"goal":"g","tasks":[{}]}}"#, entries.join(","));
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

    fn set(&self, run: &str, task: &str, status: &str) -> Output {
        self.condukt(&[
            "state", "set", "--run", run, "--task", task, "--status", status,
        ])
    }

    /// Drive `task` to `running`, which is what makes it CLAIM its files, and
    /// assert that the claim actually landed — otherwise every later assertion
    /// about releasing it would be vacuous.
    fn claim_via_running(&self, run: &str, task: &str, file: &str) {
        let out = self.set(run, task, "running");
        assert!(
            out.status.success(),
            "precondition: {run}/{task} must go running (and claim {file}): \
             stdout={} stderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let held = self.held_files(run);
        assert!(
            held.contains(&file.to_string()),
            "precondition: {run} must hold {file} after going running, holds {held:?}"
        );
    }

    /// The live registry as the CLI reports it (`state claims` prints
    /// `claim::active_claims`, i.e. after stale reaping).
    fn claims(&self) -> serde_json::Value {
        let out = self.condukt(&["state", "claims"]);
        assert!(out.status.success(), "state claims failed: {out:?}");
        serde_json::from_slice(&out.stdout).expect("state claims must emit JSON")
    }

    /// The FILE claims currently held by `run`, sorted. The file table is
    /// `#[serde(flatten)]`ed to the registry's top level, so accept either the
    /// flattened shape or an explicit `files` object rather than pinning one.
    fn held_files(&self, run: &str) -> Vec<String> {
        let v = self.claims();
        let table = v
            .get("files")
            .and_then(|f| f.as_object())
            .cloned()
            .or_else(|| {
                v.as_object().map(|o| {
                    o.iter()
                        .filter(|(k, val)| {
                            k.as_str() != "task_claims" && val.get("run_id").is_some()
                        })
                        .map(|(k, val)| (k.clone(), val.clone()))
                        .collect()
                })
            });
        let mut out: Vec<String> = table
            .unwrap_or_default()
            .iter()
            .filter(|(_, c)| c.get("run_id").and_then(|r| r.as_str()) == Some(run))
            .map(|(k, _)| k.clone())
            .collect();
        out.sort();
        out
    }

    fn run_state_path(&self, run: &str) -> PathBuf {
        let target = format!("{run}.json");
        let state_root = self.home.join(".condukt").join("state");
        find_file(&state_root, &target)
            .unwrap_or_else(|| panic!("run-state file {target} not found under {state_root:?}"))
    }

    fn read_state(&self, run: &str) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(self.run_state_path(run)).unwrap()).unwrap()
    }

    fn write_state(&self, run: &str, val: &serde_json::Value) {
        std::fs::write(
            self.run_state_path(run),
            serde_json::to_string_pretty(val).unwrap(),
        )
        .unwrap();
    }

    fn status_of(&self, run: &str, task: &str) -> String {
        let v = self.read_state(run);
        v["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == task)
            .unwrap_or_else(|| panic!("no task {task} in {run}"))["status"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// The saved decomposition the binary reads back (`<run>.decomposition.json`
    /// under the sandboxed HOME). Found by name so the private project-key path
    /// derivation is not duplicated here.
    fn decomposition_path(&self, run: &str) -> PathBuf {
        let target = format!("{run}.decomposition.json");
        let state_root = self.home.join(".condukt").join("state");
        find_file(&state_root, &target)
            .unwrap_or_else(|| panic!("decomposition {target} not found under {state_root:?}"))
    }

    /// Commit on a task branch and merge it into `main`, so
    /// `state::reconcile_run`'s "branch is an ancestor of the default branch →
    /// auto-verify" path fires. Returns the branch tip SHA.
    fn merge_task_branch(&self, branch: &str, marker: &str) -> String {
        run_git(&self.repo, &["checkout", "-q", "-b", branch]);
        std::fs::write(self.repo.join(marker), "work\n").unwrap();
        run_git(&self.repo, &["add", "."]);
        run_git(&self.repo, &["commit", "-q", "-m", "task work"]);
        let sha = String::from_utf8(
            Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&self.repo)
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();
        run_git(&self.repo, &["checkout", "-q", "main"]);
        run_git(
            &self.repo,
            &["merge", "-q", "--no-ff", branch, "-m", "merge"],
        );
        sha
    }

    /// Point `task` at an already-merged branch and mark it `done`, the state
    /// reconcile promotes to `verified`.
    fn park_done_on_merged_branch(&self, run: &str, task: &str, branch: &str, sha: &str) {
        let mut v = self.read_state(run);
        let arr = v["tasks"].as_array_mut().unwrap();
        let t = arr
            .iter_mut()
            .find(|t| t["id"] == task)
            .unwrap_or_else(|| panic!("no task {task} in {run}"));
        t["status"] = serde_json::json!("done");
        t["branch"] = serde_json::json!(branch);
        t["branch_sha"] = serde_json::json!(sha);
        self.write_state(run, &v);
    }
}

fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    let rd = std::fs::read_dir(dir).ok()?;
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_dir() {
            if let Some(hit) = find_file(&p, name) {
                return Some(hit);
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

fn text(out: &Output) -> String {
    format!(
        "--- stdout ---\n{}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

// ── (a) reconcile ─────────────────────────────────────────────────────────

/// `state reconcile` promotes a merged task to `Verified` — a terminal status —
/// so the task's file claims must be handed back. Today `reconcile_run` writes
/// the status and returns, and nothing in the `StateAction::Reconcile` handler
/// touches the registry, so the files stay held forever (until the TTL reap).
#[test]
fn reconcile_to_verified_releases_file_claims() {
    let fx = Fixture::new("reconcile");
    let dec = fx.write_decomp("dec.json", &[("t1", "src/x.rs")]);
    fx.init("runR", &dec);
    fx.claim_via_running("runR", "t1", "src/x.rs");

    let sha = fx.merge_task_branch("condukt/runR-t1", "x.txt");
    fx.park_done_on_merged_branch("runR", "t1", "condukt/runR-t1", &sha);

    let out = fx.condukt(&["state", "reconcile", "--run", "runR"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "precondition: reconcile must run cleanly here\n{}",
        text(&out)
    );
    assert_eq!(
        fx.status_of("runR", "t1"),
        "verified",
        "precondition: reconcile must have driven t1 terminal\n{}",
        text(&out)
    );

    assert_eq!(
        fx.held_files("runR"),
        Vec::<String>::new(),
        "reconcile drove t1 to the terminal status `verified`, so its file claims \
         must be released; they are still held\n{}",
        text(&out)
    );
}

// ── (b) cancel ────────────────────────────────────────────────────────────

/// `state cancel` writes `Status::Cancelled` — terminal, and accepted by the
/// completion gate — so it must hand back the task's file claims. The `state
/// set` handler releases on exactly this status; the `cancel` handler does not.
#[test]
fn cancel_releases_file_claims() {
    let fx = Fixture::new("cancel");
    let dec = fx.write_decomp("dec.json", &[("t1", "src/y.rs")]);
    fx.init("runC", &dec);
    fx.claim_via_running("runC", "t1", "src/y.rs");

    let out = fx.condukt(&["state", "cancel", "--run", "runC", "--task", "t1"]);
    assert!(
        out.status.success(),
        "precondition: cancel must succeed\n{}",
        text(&out)
    );
    assert_eq!(
        fx.status_of("runC", "t1"),
        "cancelled",
        "precondition: the task must actually be cancelled\n{}",
        text(&out)
    );

    assert_eq!(
        fx.held_files("runC"),
        Vec::<String>::new(),
        "`state cancel` reached the terminal status `cancelled` — the same status \
         `state set` releases on — so the claim must be handed back; it is still \
         held\n{}",
        text(&out)
    );
}

// ── (c) discard ───────────────────────────────────────────────────────────

/// `worktree discard` writes `Status::Discarded` — terminal ("resolved by
/// learning", accepted by `gate_reasons`) — so it must release too. Note this
/// gap is doubled: `discard_experiment` never releases, AND the `state set`
/// release gate omits `Discarded` from its match, so there is no second chance.
#[test]
fn discard_releases_file_claims() {
    let fx = Fixture::new("discard");
    let dec = fx.write_decomp("dec.json", &[("t1", "src/z.rs")]);
    fx.init("runD", &dec);
    fx.claim_via_running("runD", "t1", "src/z.rs");

    let out = fx.condukt(&["worktree", "discard", "--run", "runD", "--task", "t1"]);
    assert!(
        out.status.success(),
        "precondition: discard must succeed\n{}",
        text(&out)
    );
    assert_eq!(
        fx.status_of("runD", "t1"),
        "discarded",
        "precondition: the task must actually be discarded\n{}",
        text(&out)
    );

    assert_eq!(
        fx.held_files("runD"),
        Vec::<String>::new(),
        "`worktree discard` reached the terminal status `discarded`, so the claim \
         must be handed back; it is still held\n{}",
        text(&out)
    );
}

/// The second half of the discard gap, isolated: even when the status is set
/// through the ONE handler that does release (`state set`), `Discarded` is
/// missing from its `matches!` gate, so nothing is released.
#[test]
fn state_set_discarded_releases_file_claims() {
    let fx = Fixture::new("set-discarded");
    let dec = fx.write_decomp("dec.json", &[("t1", "src/w.rs")]);
    fx.init("runS", &dec);
    fx.claim_via_running("runS", "t1", "src/w.rs");

    let out = fx.set("runS", "t1", "discarded");
    assert!(
        out.status.success(),
        "precondition: `state set --status discarded` must be accepted\n{}",
        text(&out)
    );
    assert_eq!(
        fx.status_of("runS", "t1"),
        "discarded",
        "precondition: the durable status must be discarded\n{}",
        text(&out)
    );

    assert_eq!(
        fx.held_files("runS"),
        Vec::<String>::new(),
        "`discarded` is terminal everywhere else (gate_reasons, reconcile_run, \
         wt_reconcile) but is missing from the release gate's `matches!`, so the \
         claim survives a terminal transition\n{}",
        text(&out)
    );
}

// ── (d) completion gate ───────────────────────────────────────────────────

/// The end-to-end shape recorded on the ticket: every task terminal, `state
/// gate` reports PASS — and the run is still holding its files.
///
/// Stated as an IMPLICATION rather than a recipe, so it is satisfied by either
/// honest fix: release on every terminal path (nothing left by gate time), or a
/// gate that refuses to report PASS while claims remain. What it forbids is the
/// combination observed today: PASS *and* claims held.
#[test]
fn completion_gate_pass_implies_zero_file_claims() {
    let fx = Fixture::new("gate");
    let dec = fx.write_decomp("dec.json", &[("t1", "src/a.rs"), ("t2", "src/b.rs")]);
    fx.init("runG", &dec);
    fx.claim_via_running("runG", "t1", "src/a.rs");
    fx.claim_via_running("runG", "t2", "src/b.rs");

    // t1 reaches a terminal status via reconcile (merged branch → verified),
    // t2 via cancel. Both are statuses the completion gate accepts, and
    // NEITHER path releases today.
    let sha = fx.merge_task_branch("condukt/runG-t1", "a.txt");
    fx.park_done_on_merged_branch("runG", "t1", "condukt/runG-t1", &sha);
    let rec = fx.condukt(&["state", "reconcile", "--run", "runG"]);
    assert_eq!(
        rec.status.code(),
        Some(0),
        "precondition: reconcile must run cleanly\n{}",
        text(&rec)
    );
    let can = fx.condukt(&["state", "cancel", "--run", "runG", "--task", "t2"]);
    assert!(
        can.status.success(),
        "precondition: cancel must succeed\n{}",
        text(&can)
    );

    let gate = fx.condukt(&["state", "gate", "--run", "runG"]);
    let held = fx.held_files("runG");
    if gate.status.code() == Some(0) {
        assert_eq!(
            held,
            Vec::<String>::new(),
            "the completion gate reported PASS for run 'runG' while it still holds \
             {} file claim(s) {held:?} — exactly the state recorded on backlog \
             06eb8aa3 (3/3 verified, gate PASS, 13 files still held and released by \
             hand)\n--- gate ---\n{}",
            held.len(),
            text(&gate)
        );
    } else {
        // The other honest resolution: the gate refuses to pass while claims
        // are outstanding. Then it must SAY which ones.
        let all = text(&gate);
        for f in &held {
            assert!(
                all.contains(f.as_str()),
                "the gate failed while claims are outstanding, which is a fine \
                 resolution — but it must name the file that stayed held ({f}); \
                 it did not\n{all}"
            );
        }
    }
}

// ── (e) failure must be loud, not an empty set ────────────────────────────

/// `task_files` collapses "the decomposition cannot be read" into the same
/// empty `Vec` as "this task touches no files", and the caller's `if
/// !files.is_empty()` then skips the release with no output at all. The claim
/// survives a terminal transition and NOTHING says so.
///
/// `--status failed` is used deliberately: `--status verified` bails out on an
/// unreadable decomposition at the fail-to-pass gate (`FpGateScope::Undetermined`)
/// before the release block is ever reached, so it cannot observe this arm.
#[test]
fn unreadable_decomposition_terminal_set_is_not_silent() {
    let fx = Fixture::new("undet-loud");
    let dec = fx.write_decomp("dec.json", &[("t1", "src/shared.rs")]);
    fx.init("runU", &dec);
    fx.claim_via_running("runU", "t1", "src/shared.rs");

    // Fault injection: make the SAVED decomposition unparseable, so
    // `task_files` cannot determine which files this task holds.
    std::fs::write(fx.decomposition_path("runU"), b"{ not json ]]").unwrap();

    let out = fx.set("runU", "t1", "failed");
    let all = text(&out);

    // Premise of the test: the release genuinely did not happen.
    assert_eq!(
        fx.held_files("runU"),
        vec!["src/shared.rs".to_string()],
        "premise: with an unreadable decomposition the claim is NOT released \
         (if this ever fails, the loudness assertion below is testing nothing)\n{all}"
    );
    assert_eq!(
        fx.status_of("runU", "t1"),
        "failed",
        "precondition: the terminal transition itself must have happened\n{all}"
    );

    assert!(
        all.contains("src/shared.rs"),
        "the release was skipped because the decomposition could not be read, and \
         the run kept holding src/shared.rs — but the command named nothing. \
         'Cannot determine which files to release' must not be reported as \
         'nothing to release' (CLAUDE.md §3: an error must not return an empty \
         set)\n{all}"
    );
    assert!(
        all.contains("runU"),
        "the message must name the run whose claims stayed held\n{all}"
    );
}

/// The exit-code half of the same contract, isolated because it is the half
/// that contradicts `tests/heartbeat_err_surfaced.rs`
/// (`state_write_stands_and_exit_code_is_zero_when_upkeep_fails` /
/// `release_files_failure_is_named_on_stderr` both pin exit 0 for a `state set`
/// whose claim upkeep fails, on the grounds that the durable state write
/// already succeeded).
///
/// Backlog `06eb8aa3`'s done_criteria demands the opposite: "a release that
/// CANNOT be completed must NOT be silent: it must NAME what stayed held and
/// exit NON-ZERO." Both cannot hold. This test asserts the ticket's side; the
/// implementer must resolve the conflict explicitly — either by changing the
/// frozen contract deliberately, or by carrying the non-zero exit somewhere
/// that does not lie about the state write.
#[test]
#[ignore = "DEFERRED TO A HUMAN, not weakened: this assertion contradicts the frozen \
            exit-0 contract at tests/heartbeat_err_surfaced.rs:359 \
            (`release_files_failure_is_named_on_stderr`, whose message is unhedged — \
            \"same contract as the heartbeat half: the state write stands\") and the \
            reasoning for it in main.rs's claim-upkeep block. Both cannot hold for \
            `state set`: one requires exit 0 after a failed claim hand-back, this one \
            requires non-zero. The implementer of backlog 06eb8aa3 closed the four \
            release gaps and made this path LOUD (see \
            `unreadable_decomposition_terminal_set_is_not_silent`, which passes) but did \
            NOT pick a winner on the exit code, because picking one silently would be \
            choosing which frozen contract to break. The body is kept intact so the \
            contract it asks for stays readable in the tree. See backlog 06eb8aa3."]
fn unreadable_decomposition_terminal_set_exits_non_zero() {
    let fx = Fixture::new("undet-exit");
    let dec = fx.write_decomp("dec.json", &[("t1", "src/shared.rs")]);
    fx.init("runX", &dec);
    fx.claim_via_running("runX", "t1", "src/shared.rs");

    std::fs::write(fx.decomposition_path("runX"), b"{ not json ]]").unwrap();

    let out = fx.set("runX", "t1", "failed");
    let all = text(&out);

    assert_eq!(
        fx.held_files("runX"),
        vec!["src/shared.rs".to_string()],
        "premise: the claim must actually have stayed held for the exit code to \
         be wrong\n{all}"
    );
    assert_ne!(
        out.status.code(),
        Some(0),
        "the run still holds src/shared.rs after a terminal transition, and the \
         command reported success — a caller (and the completion gate downstream) \
         cannot tell a released run from a stranded one\n{all}"
    );
}

/// ANTI-VACUITY CONTROL for the two tests above: with an INTACT decomposition
/// the same terminal transition exits 0, says nothing about stranded claims,
/// and the claim is demonstrably gone. Without this, both tests above would
/// also pass against an implementation that shouts unconditionally.
#[test]
fn intact_decomposition_terminal_set_is_quiet_and_releases() {
    let fx = Fixture::new("undet-control");
    let dec = fx.write_decomp("dec.json", &[("t1", "src/shared.rs")]);
    fx.init("runOK", &dec);
    fx.claim_via_running("runOK", "t1", "src/shared.rs");

    let out = fx.set("runOK", "t1", "failed");
    let all = text(&out);
    assert_eq!(
        out.status.code(),
        Some(0),
        "a healthy terminal transition must still exit 0\n{all}"
    );
    assert_eq!(
        fx.held_files("runOK"),
        Vec::<String>::new(),
        "the control is vacuous unless the release demonstrably ran on this \
         path\n{all}"
    );
    assert!(
        !all.contains("src/shared.rs"),
        "a healthy release must not report a stranded file, or the loudness \
         assertions prove nothing\n{all}"
    );
}

// ── (f) regression guard: `done` is NOT terminal ──────────────────────────

/// GREEN BEFORE THE FIX, on purpose. `Status::Done` means "the worker finished,
/// verification has not happened yet" — the task still owns its files and a
/// second session must not be able to take them. A fix that widens the release
/// gate must not widen it to `Done`.
#[test]
fn done_status_keeps_its_claims() {
    let fx = Fixture::new("done-guard");
    let dec = fx.write_decomp("dec.json", &[("t1", "src/keep.rs")]);
    fx.init("runK", &dec);
    fx.claim_via_running("runK", "t1", "src/keep.rs");

    let out = fx.set("runK", "t1", "done");
    assert!(
        out.status.success(),
        "precondition: `state set --status done` must succeed\n{}",
        text(&out)
    );
    assert_eq!(
        fx.status_of("runK", "t1"),
        "done",
        "precondition: the durable status must be done\n{}",
        text(&out)
    );

    assert_eq!(
        fx.held_files("runK"),
        vec!["src/keep.rs".to_string()],
        "`done` is NOT terminal — it awaits verification — so the task must KEEP \
         its file claims. Releasing here would hand a verifier's files to another \
         session mid-verification\n{}",
        text(&out)
    );
}
