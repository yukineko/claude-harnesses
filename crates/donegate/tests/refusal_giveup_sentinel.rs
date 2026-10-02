// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! End-to-end: donegate's **UNDETERMINED** give-up must leave a durable
//! sentinel too (backlog a5bc063a).
//!
//! donegate has two give-up paths and only one of them is observable from
//! outside the process:
//!
//! * **checks-red give-up** — the gate judged, the required checks were still
//!   failing at the attempt cap. Pinned by `tests/giveup_sentinel.rs`; writes
//!   `donegate:giveup:<check>` to the overwatch violation ledger.
//! * **undetermined give-up** — the gate could not judge AT ALL
//!   (`Declaration::is_refusal()`: the project is not trusted, or the config
//!   could not be read) and `refuse()` hit the same attempt cap. Measured
//!   2026-10-02 on the unmodified binary: the only traces are a line in
//!   donegate's own private `~/.donegate/state/log.jsonl` and a line on stderr.
//!   Nothing else reads either. The overwatch ledger file is never even
//!   created.
//!
//! So the HEAVIER case ("I could not check anything") is the invisible one,
//! while the lighter case ("I checked, it is red, I am standing down") is
//! durable. Downstream, an undetermined give-up is indistinguishable from a
//! clean pass — the exact state the checks-red sentinel exists to rule out.
//!
//! These tests pin the required behaviour, not an implementation: they observe
//! only the ledger on disk and compare SETS of signatures against each other,
//! so they do not transcribe whatever wording the fix settles on.
//!
//! Apparatus is deliberately a copy of `tests/giveup_sentinel.rs` (isolated
//! `$HOME` per child process, real built binary, ledger read straight off disk
//! rather than through the code under test) so the two give-up paths are
//! observed through the same instrument.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// `max_attempts` every fixture in this file runs under. The refusal paths
/// cannot carry a project `max_attempts` (an untrusted project's check set is
/// discarded, and an unreadable config parses to nothing), so this is the
/// built-in default (`Config::default`), asserted indirectly by the apparatus
/// checks below: if the default ever changes, the "under the cap" loop stops
/// matching and the tests fail loudly instead of silently skipping the cap.
const MAX_ATTEMPTS: u32 = 3;

fn scratch(tag: &str) -> PathBuf {
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("donegate-refusal-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch");
    dir
}

fn trust(home: &Path, root: &Path) {
    // `trust::is_trusted` canonicalizes before comparing, so the seeded entry
    // must be the canonical form.
    let canon = std::fs::canonicalize(root).expect("canonicalize project root");
    std::fs::create_dir_all(home.join(".harness")).expect("create ~/.harness");
    std::fs::write(
        home.join(".harness/trust.toml"),
        format!("trusted = [\"{}\"]\n", canon.display()),
    )
    .expect("write trust.toml");
}

/// `Declaration::RefusedUntrusted`: the project declares checks but is NOT on
/// the workspace-trust list, so donegate refuses to run them. No `trust.toml`
/// is written — that omission IS the fixture.
fn untrusted_project(tag: &str) -> (PathBuf, PathBuf) {
    let home = scratch(tag);
    let root = home.join("project");
    std::fs::create_dir_all(&root).expect("create project root");
    std::fs::write(
        root.join("donegate.toml"),
        "[[check]]\nname = \"typecheck\"\ncmd = \"exit 1\"\n",
    )
    .expect("write donegate.toml");
    (home, root)
}

/// `Declaration::Unreadable`: the project IS trusted, but its config cannot be
/// parsed, so donegate cannot know what it was supposed to check.
fn unreadable_project(tag: &str) -> (PathBuf, PathBuf) {
    let home = scratch(tag);
    let root = home.join("project");
    std::fs::create_dir_all(&root).expect("create project root");
    std::fs::write(root.join("donegate.toml"), "this is not = = toml [[[\n")
        .expect("write donegate.toml");
    trust(&home, &root);
    (home, root)
}

/// A trusted project with one required check named `name` that always fails.
/// This is the CHECKS-RED path (`tests/giveup_sentinel.rs`'s fixture), present
/// here only as the comparison set for the disjointness assertions.
fn red_project(tag: &str, name: &str) -> (PathBuf, PathBuf) {
    let home = scratch(tag);
    let root = home.join("project");
    std::fs::create_dir_all(&root).expect("create project root");
    let escaped = name.replace('\\', "\\\\").replace('"', "\\\"");
    std::fs::write(
        root.join("donegate.toml"),
        format!(
            "max_attempts = {MAX_ATTEMPTS}\n\n[[check]]\nname = \"{escaped}\"\ncmd = \"exit 1\"\n"
        ),
    )
    .expect("write donegate.toml");
    trust(&home, &root);
    (home, root)
}

struct GateRun {
    code: i32,
    stdout: String,
    stderr: String,
}

impl GateRun {
    fn blocked(&self) -> bool {
        self.stdout.contains("\"decision\"") && self.stdout.contains("block")
    }
}

/// One Stop hook invocation for `session`. `CLAUDE_CODE_SESSION_ID` is set to
/// the same session as the payload (the repeat ledger keys on the env var, not
/// on the payload) and must not be inherited from whatever ran `cargo test`.
fn run_gate(home: &Path, root: &Path, session: &str) -> GateRun {
    let bin = env!("CARGO_BIN_EXE_donegate");
    let payload = format!(
        r#"{{"session_id":"{session}","cwd":"{}","hook_event_name":"Stop"}}"#,
        root.display()
    );
    let mut child = Command::new(bin)
        .arg("gate")
        .current_dir(root)
        .env("HOME", home)
        .env("CLAUDE_CODE_SESSION_ID", session)
        .env_remove("DONEGATE_DISABLE")
        .env_remove("HARNESS_TRUST_ALL")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("donegate spawns");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("write payload");
    let out = child.wait_with_output().expect("donegate runs");
    GateRun {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// Concatenated contents of every `violations.jsonl` under the isolated
/// `$HOME`, read straight off disk so the observation does not depend on the
/// code under test.
fn violation_ledger(home: &Path) -> String {
    fn walk(dir: &Path, out: &mut String) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.file_name().is_some_and(|n| n == "violations.jsonl") {
                out.push_str(&std::fs::read_to_string(&p).unwrap_or_default());
            }
        }
    }
    let mut out = String::new();
    walk(home, &mut out);
    out
}

/// Every `violations.jsonl` file under the isolated `$HOME`, sorted. The PATH
/// matters as well as the content: `overwatch::store::scan_violations(root)`
/// resolves one specific per-project file, so a sentinel written under a
/// different project key is still invisible to the command a reader runs.
fn ledger_paths(home: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.file_name().is_some_and(|n| n == "violations.jsonl") {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    walk(home, &mut out);
    out.sort();
    out
}

/// Every donegate-sourced violation event in the isolated `$HOME`'s ledger.
fn donegate_events(home: &Path) -> Vec<overwatch::violation::ViolationEvent> {
    violation_ledger(home)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            serde_json::from_str::<overwatch::violation::ViolationEvent>(l).unwrap_or_else(|e| {
                panic!("a line in the violation ledger did not parse ({e}): {l:?}")
            })
        })
        .filter(|e| e.source == overwatch::violation::ViolationSource::Donegate)
        .collect()
}

/// The distinct signatures donegate recorded under this `$HOME`.
fn donegate_signatures(home: &Path) -> BTreeSet<String> {
    donegate_events(home)
        .into_iter()
        .map(|e| e.signature)
        .collect()
}

/// Drive `MAX_ATTEMPTS + 1` Stops against a project donegate CANNOT JUDGE, so
/// `refuse()`'s attempt cap is crossed exactly once.
///
/// Asserts the shape of every stop on the way (each under-cap stop must BLOCK
/// with the refusal text, the last must ALLOW with the give-up text), so a
/// future change that stops reaching the cap makes these tests fail rather
/// than pass vacuously. Returns the give-up run itself.
fn one_refusal_giveup_cycle(home: &Path, root: &Path, session: &str) -> GateRun {
    for i in 1..=MAX_ATTEMPTS {
        let r = run_gate(home, root, session);
        assert_eq!(r.code, 0, "a Stop hook always exits 0 toward Claude");
        assert!(
            r.blocked(),
            "apparatus: attempt {i} is under the cap and must BLOCK; stdout={:?} stderr={:?}",
            r.stdout,
            r.stderr
        );
        assert!(
            r.stdout.contains("REFUSING TO JUDGE"),
            "apparatus: attempt {i} must be the refusal path, not the checks path; stdout={:?}",
            r.stdout
        );
        assert!(
            !r.stderr.contains("NOTHING WAS VERIFIED"),
            "apparatus: attempt {i} is under the cap and must not give up; stderr={:?}",
            r.stderr
        );
    }
    let r = run_gate(home, root, session);
    assert_eq!(r.code, 0, "the give-up path exits 0");
    assert!(
        !r.blocked(),
        "IN SCOPE-CHECK: the cap must still ALLOW the stop — these tests add a sentinel, they do \
         not turn the bounded concession into a trap. stdout={:?}",
        r.stdout
    );
    assert!(
        r.stderr.contains("unable to judge") && r.stderr.contains("NOTHING WAS VERIFIED"),
        "apparatus: the undetermined give-up branch must be the one that ran; stderr={:?}",
        r.stderr
    );
    r
}

/// Drive one CHECKS-RED give-up cycle (the already-pinned path 1), used here
/// only to produce its signatures for comparison.
fn one_checks_red_giveup_cycle(home: &Path, root: &Path, session: &str) -> GateRun {
    for i in 1..=MAX_ATTEMPTS {
        let r = run_gate(home, root, session);
        assert!(
            r.blocked(),
            "apparatus: checks-red attempt {i} must block; stdout={:?} stderr={:?}",
            r.stdout,
            r.stderr
        );
    }
    let r = run_gate(home, root, session);
    assert!(
        r.stderr.contains("still failing"),
        "apparatus: the checks-red give-up branch must have run; stderr={:?}",
        r.stderr
    );
    r
}

/// THE FINDING (kind 1 of 2): donegate gave up because the project is NOT
/// TRUSTED — it ran nothing and verified nothing — and left no durable trace.
///
/// CONTROL is inline: before the cap, while the gate is still blocking, the
/// ledger must hold NO donegate event. Without it, a sentinel written on every
/// refusal would satisfy the assertion below while meaning "donegate refused",
/// not "donegate gave up".
#[test]
fn an_untrusted_refusal_that_reaches_the_cap_leaves_a_durable_sentinel() {
    let (home, root) = untrusted_project("untrusted-cap");

    for i in 1..=MAX_ATTEMPTS {
        let r = run_gate(&home, &root, "sess-untrusted");
        assert!(
            r.blocked(),
            "apparatus: attempt {i} must block; stdout={:?} stderr={:?}",
            r.stdout,
            r.stderr
        );
    }
    let under_cap = donegate_signatures(&home);
    assert!(
        under_cap.is_empty(),
        "CONTROL: while the gate is still BLOCKING on a refusal it has not given up, so it must \
         record no give-up sentinel. got: {under_cap:?}"
    );

    let r = run_gate(&home, &root, "sess-untrusted");
    assert_eq!(r.code, 0, "the give-up path exits 0");
    assert!(
        !r.blocked(),
        "IN SCOPE-CHECK: the cap must still allow the stop; stdout={:?}",
        r.stdout
    );
    assert!(
        r.stderr.contains("NOTHING WAS VERIFIED"),
        "apparatus: the undetermined give-up branch must have run; stderr={:?}",
        r.stderr
    );

    let after = donegate_events(&home);
    assert_eq!(
        after.len(),
        1,
        "donegate gave up WITHOUT HAVING JUDGED ANYTHING and left no durable trace, so \
         'the gate could not check' is indistinguishable from 'the gate passed' for every \
         downstream reader. Exactly one overwatch violation event is required (one give-up = one \
         occurrence; emitting several would inflate `detect_recurrence`'s occurrence count for a \
         single event). ledger={:?}",
        violation_ledger(&home)
    );
    assert_eq!(
        after[0].source,
        overwatch::violation::ViolationSource::Donegate,
        "the sentinel must be attributed to donegate"
    );
}

/// THE FINDING (kind 2 of 2): same give-up, different reason — the config
/// could not be read, so donegate does not even know what it was supposed to
/// check. Separate test because the two refusal kinds reach the cap through
/// different `Declaration` arms and nothing guarantees a fix covers both.
#[test]
fn an_unreadable_config_refusal_that_reaches_the_cap_leaves_a_durable_sentinel() {
    let (home, root) = unreadable_project("unreadable-cap");

    for i in 1..=MAX_ATTEMPTS {
        let r = run_gate(&home, &root, "sess-unreadable");
        assert!(
            r.blocked(),
            "apparatus: attempt {i} must block; stdout={:?} stderr={:?}",
            r.stdout,
            r.stderr
        );
    }
    let under_cap = donegate_signatures(&home);
    assert!(
        under_cap.is_empty(),
        "CONTROL: a refusal that is still blocking has not given up and must record nothing. \
         got: {under_cap:?}"
    );

    let r = run_gate(&home, &root, "sess-unreadable");
    assert!(
        r.stderr.contains("NOTHING WAS VERIFIED"),
        "apparatus: the undetermined give-up branch must have run; stderr={:?}",
        r.stderr
    );

    let after = donegate_events(&home);
    assert_eq!(
        after.len(),
        1,
        "donegate gave up on a config it could not read and left no durable trace; an unreadable \
         config may declare ten required checks, so this give-up is strictly heavier than a \
         checks-red one — and it is the one with no sentinel. ledger={:?}",
        violation_ledger(&home)
    );
}

/// An undetermined give-up and a judged give-up must not land in the same
/// overwatch recurrence bucket: `detect_recurrence` buckets purely by
/// signature, so a shared signature would merge "the checks are red" with
/// "nothing was checked" into one systemic issue with one remedy.
///
/// Asserted as set disjointness rather than against a literal string, so the
/// fix is free to choose its own wording.
///
/// Also the PATH-1 REGRESSION GUARD: the checks-red half of the comparison is
/// asserted to still carry `donegate:giveup:<check>` and `donegate:<check>`.
#[test]
fn the_undetermined_giveup_bucket_is_disjoint_from_the_checks_red_buckets() {
    let (home_r, root_r) = untrusted_project("disjoint-refusal");
    one_refusal_giveup_cycle(&home_r, &root_r, "sess-r");
    let refusal = donegate_signatures(&home_r);

    let (home_c, root_c) = red_project("disjoint-checks", "typecheck");
    one_checks_red_giveup_cycle(&home_c, &root_c, "sess-c");
    let checks_red = donegate_signatures(&home_c);

    // PATH-1 REGRESSION GUARD (and apparatus: without these the disjointness
    // below could hold by both sets being empty).
    assert!(
        checks_red.contains("donegate:giveup:typecheck"),
        "REGRESSION: the checks-red give-up sentinel must be unchanged. got: {checks_red:?}"
    );
    assert!(
        checks_red.contains("donegate:typecheck"),
        "REGRESSION: ordinary blocks must still be recorded. got: {checks_red:?}"
    );

    assert!(
        !refusal.is_empty(),
        "an undetermined give-up recorded nothing at all, so there is no bucket to keep separate. \
         ledger={:?}",
        violation_ledger(&home_r)
    );
    let shared: Vec<_> = refusal.intersection(&checks_red).collect();
    assert!(
        shared.is_empty(),
        "'nothing was checked' and 'the checks are red' share an overwatch recurrence bucket \
         {shared:?}; they have different remedies and must not correlate as one issue. \
         refusal={refusal:?} checks_red={checks_red:?}"
    );
}

/// The signature must say WHY the gate could not judge. `RefusedUntrusted` is
/// fixed by `donegate trust`; `Unreadable` is fixed by repairing a file.
/// Collapsing them into one bucket makes the ledger say "donegate is stuck"
/// without saying what to do about it, and makes two unrelated projects'
/// problems escalate as one systemic issue.
#[test]
fn the_two_refusal_kinds_do_not_share_a_bucket() {
    let (home_u, root_u) = untrusted_project("kinds-untrusted");
    one_refusal_giveup_cycle(&home_u, &root_u, "sess-u");
    let untrusted = donegate_signatures(&home_u);

    let (home_p, root_p) = unreadable_project("kinds-unreadable");
    one_refusal_giveup_cycle(&home_p, &root_p, "sess-p");
    let unreadable = donegate_signatures(&home_p);

    assert!(
        !untrusted.is_empty() && !unreadable.is_empty(),
        "apparatus: both refusal kinds must record something before they can be distinguished. \
         untrusted={untrusted:?} unreadable={unreadable:?}"
    );
    assert_ne!(
        untrusted, unreadable,
        "an untrusted project and an unreadable config gave up into the SAME overwatch bucket, \
         so the ledger cannot tell a reader which remedy applies (`donegate trust` vs fixing a \
         broken file)"
    );
    let shared: Vec<_> = untrusted.intersection(&unreadable).collect();
    assert!(
        shared.is_empty(),
        "the two refusal kinds share the bucket(s) {shared:?}; recurrence would correlate two \
         unrelated causes. untrusted={untrusted:?} unreadable={unreadable:?}"
    );
}

/// Every signature a hostile project that declares a check literally named
/// `name` can get donegate to write, driven to the attempt cap.
///
/// Deliberately AGNOSTIC about which give-up the project ends up taking. There
/// are two legitimate ways for a name to be un-forgeable and the property under
/// test does not care which is used:
///
/// * the name is declarable and simply lands in a different bucket, or
/// * the name is REJECTED at config load, so the check never runs at all.
///
/// What it is NOT agnostic about is the gate going quiet. Both of the above
/// must still BLOCK every stop under the cap. The cheap way to make a reserved
/// name un-declarable is to drop the offending `[[check]]` during
/// sanitization — after which the project declares zero checks, donegate says
/// "nothing to do" and ALLOWS on the very first stop. That is a gate made
/// weaker by a naming collision, i.e. the fail-open class this whole sentinel
/// exists to end. The per-attempt `blocked()` assertion below is what rules it
/// out, so this helper is a weaker apparatus than
/// [`one_checks_red_giveup_cycle`] in exactly one dimension (which branch gave
/// up) and no others.
fn signatures_a_project_named_check_can_produce(
    tag: &str,
    name: &str,
) -> (BTreeSet<String>, String) {
    let (home, root) = red_project(tag, name);
    for i in 1..=MAX_ATTEMPTS {
        let r = run_gate(&home, &root, "sess-forge");
        assert_eq!(r.code, 0, "a Stop hook always exits 0 toward Claude");
        assert!(
            r.blocked(),
            "a project declaring a check named {name:?} was ALLOWED to stop on attempt {i} \
             instead of being blocked. If the name is reserved it must be REFUSED (block), never \
             dropped — a dropped check is a gate silently made weaker by a naming collision. \
             stdout={:?} stderr={:?}",
            r.stdout,
            r.stderr
        );
    }
    let r = run_gate(&home, &root, "sess-forge");
    assert!(
        r.stderr.contains("still failing") || r.stderr.contains("unable to judge"),
        "apparatus: the cap must have been reached by one give-up branch or the other; \
         stderr={:?}",
        r.stderr
    );
    let sigs = donegate_signatures(&home);
    assert!(
        !sigs.is_empty(),
        "apparatus: the forging project must have recorded something, else the absence asserted \
         by the caller is vacuous. stderr={:?}",
        r.stderr
    );
    (sigs, r.stderr)
}

/// The check names a project would have to declare to land in `sig`'s bucket:
/// the signature minus its `donegate:` source prefix, and (if the sentinel was
/// placed in the checks-red outcome namespace) minus a `giveup:` prefix too,
/// since `donegate:giveup:X` is what a check named `X` produces when it gives
/// up AND what a check named `giveup:X` produces when it merely blocks.
fn forging_candidates(sig: &str) -> Vec<String> {
    let disc = sig
        .strip_prefix("donegate:")
        .unwrap_or_else(|| panic!("a donegate signature must start with `donegate:`: {sig}"));
    let mut out = vec![disc.to_string()];
    if let Some(inner) = disc.strip_prefix("giveup:") {
        out.push(inner.to_string());
    }
    out
}

/// ANTI-VACUITY CONTROL for the test below, executed rather than argued.
///
/// The relaxed apparatus in [`signatures_a_project_named_check_can_produce`]
/// accepts "the forging project could not even declare that check" as a pass.
/// That relaxation is only legitimate if the detector still catches the thing
/// it was written to catch: an undetermined sentinel emitted through the
/// checks-red emitter under a synthetic name.
///
/// So this simulates exactly that implementation — the sentinel
/// `donegate:giveup:no-verdict`, which is what
/// `emit_violations(.., &["no-verdict"], Outcome::GaveUp)` would write — and
/// runs the real derivation and the real detector against the REAL binary.
/// A project declaring that check name is not reserved, is not rejected, runs,
/// and reaches the identical signature. The detector must see it.
#[test]
fn the_forgery_detector_catches_a_sentinel_in_the_checks_red_namespace() {
    let lazy_sentinel = "donegate:giveup:no-verdict";

    let candidates = forging_candidates(lazy_sentinel);
    assert!(
        candidates.iter().any(|c| c == "no-verdict"),
        "derivation: the synthetic check name must be among the candidates tried; got \
         {candidates:?}"
    );

    let mut caught_by = Vec::new();
    for candidate in &candidates {
        let (reachable, _) = signatures_a_project_named_check_can_produce("teeth", candidate);
        if reachable.contains(lazy_sentinel) {
            caught_by.push(candidate.clone());
        }
    }
    assert!(
        !caught_by.is_empty(),
        "the detector would NOT have caught an undetermined sentinel placed in the \
         `donegate:giveup:<name>` namespace, so the negative result in \
         `a_project_check_cannot_forge_the_undetermined_sentinel` proves nothing. \
         candidates={candidates:?}"
    );
}

/// The undetermined sentinel must live in a namespace a PROJECT CANNOT FORGE.
///
/// The cheap way to add this sentinel is to reuse the checks-red emitter with a
/// synthetic check name, which puts it in the same namespace real check names
/// occupy. Then any project with a check of that name produces a
/// byte-identical signature, and "nothing was checked" becomes
/// indistinguishable from "this particular check is red" again — the exact
/// collapse the ticket is about, reintroduced one level down.
///
/// The adversarial check name is DERIVED from whatever signature the fix emits,
/// so this test does not depend on the wording chosen. Nor does it depend on
/// HOW the name is made unusable: being rejected at config load satisfies the
/// property at least as strongly as landing in a separate bucket (the hostile
/// signature is then not merely different, it is unreachable), provided the
/// rejection blocks rather than quietly dropping the check — which
/// [`signatures_a_project_named_check_can_produce`] enforces.
#[test]
fn a_project_check_cannot_forge_the_undetermined_sentinel() {
    // The detector's teeth are established separately and by execution, in
    // `the_forgery_detector_catches_a_sentinel_in_the_checks_red_namespace`.
    let (home_r, root_r) = untrusted_project("forge-refusal");
    one_refusal_giveup_cycle(&home_r, &root_r, "sess-forge-r");
    let refusal = donegate_signatures(&home_r);
    assert!(
        !refusal.is_empty(),
        "an undetermined give-up recorded nothing at all, so there is no namespace to protect. \
         ledger={:?}",
        violation_ledger(&home_r)
    );

    for sig in &refusal {
        for candidate in forging_candidates(sig) {
            let (reachable, stderr) =
                signatures_a_project_named_check_can_produce("forge-checks", &candidate);
            assert!(
                !reachable.contains(sig),
                "a project that merely names a check {candidate:?} produced the undetermined \
                 sentinel {sig:?}: the sentinel is forgeable, and that project's red check is \
                 then indistinguishable from 'donegate could not judge'. reachable={reachable:?} \
                 stderr={stderr:?}"
            );
        }
    }
}

/// WHERE the sentinel is written, not just that it exists.
///
/// A reader asks `overwatch violations` from the project root, i.e.
/// `overwatch::store::scan_violations(root)`, which resolves exactly ONE
/// per-project file. A sentinel appended under a different project key is
/// durable and still invisible from the project it describes — the same
/// failure mode in a new disguise.
///
/// Pinned without recomputing the project-key hash: the SAME project root is
/// driven through an undetermined give-up (untrusted) and then, after being
/// trusted, through a checks-red give-up whose destination is already pinned
/// by `tests/giveup_sentinel.rs`. Both must land in the same file.
#[test]
fn the_undetermined_sentinel_lands_in_the_project_store_a_reader_scans() {
    let (home, root) = untrusted_project("store-key");

    one_refusal_giveup_cycle(&home, &root, "sess-undetermined");
    let after_refusal = ledger_paths(&home);
    assert_eq!(
        after_refusal.len(),
        1,
        "the undetermined give-up wrote no per-project violation ledger, so \
         `overwatch::store::scan_violations(root)` — the only sanctioned reader — returns \
         `Absent` and the give-up reads as 'no violations'. found: {after_refusal:?}"
    );

    // Same root, now trusted: its `[[check]]` runs, fails, and gives up. That
    // destination is the already-pinned one.
    trust(&home, &root);
    one_checks_red_giveup_cycle(&home, &root, "sess-checks-red");
    let after_both = ledger_paths(&home);
    assert_eq!(
        after_both, after_refusal,
        "the two give-up kinds for the SAME project root landed in different stores; the \
         undetermined one is therefore not where a reader of that project looks"
    );
}

/// A RESERVED CHECK NAME and a BROKEN TOML are two different reasons donegate
/// cannot judge, and they must not land in the same recurrence bucket.
///
/// Both are "donegate declined to act on a config file" and both resolve the
/// same way (refuse, bounded, block), which is exactly why folding them is
/// tempting. The remedies are not the same: one is fixed by renaming a check,
/// the other by repairing the file. An operator reading `overwatch violations`
/// and seeing one recurring signature would be pointed at the wrong thing, and
/// two unrelated problems would correlate as one systemic issue — the same
/// collapse this whole sentinel exists to end, re-created one level down
/// inside the fix.
///
/// The reserved name is DERIVED from the sentinel the implementation actually
/// emits (as in `a_project_check_cannot_forge_the_undetermined_sentinel`), so
/// this test does not hardcode which namespace is reserved.
#[test]
fn a_reserved_check_name_and_a_broken_config_do_not_share_a_bucket() {
    // The untrusted fixture is the source of truth for what the reserved
    // namespace looks like: its sentinel's discriminator is, by construction,
    // a string inside it.
    let (home_u, root_u) = untrusted_project("split-untrusted");
    one_refusal_giveup_cycle(&home_u, &root_u, "sess-split-u");
    let untrusted = donegate_signatures(&home_u);
    assert_eq!(
        untrusted.len(),
        1,
        "apparatus: one undetermined give-up records one signature; got {untrusted:?}"
    );
    let reserved_name = untrusted
        .iter()
        .next()
        .and_then(|s| s.strip_prefix("donegate:"))
        .expect("apparatus: a donegate signature")
        .to_string();

    // A project that declares a check with that name. It parses fine — this is
    // NOT the broken-TOML case — but donegate must decline to run it.
    let (home_r, root_r) = red_project("split-reserved", &reserved_name);
    let run = one_refusal_giveup_cycle(&home_r, &root_r, "sess-split-r");
    assert!(
        !run.stderr.contains("still failing"),
        "apparatus: the reserved-name project must reach the REFUSAL give-up, not the \
         checks-red one — otherwise this test is comparing the wrong two things. stderr={:?}",
        run.stderr
    );
    let reserved = donegate_signatures(&home_r);

    // A config that genuinely cannot be parsed.
    let (home_b, root_b) = unreadable_project("split-broken");
    one_refusal_giveup_cycle(&home_b, &root_b, "sess-split-b");
    let broken = donegate_signatures(&home_b);

    assert!(
        !reserved.is_empty() && !broken.is_empty(),
        "apparatus: both causes must record something before they can be distinguished. \
         reserved={reserved:?} broken={broken:?}"
    );

    let shared: Vec<_> = reserved.intersection(&broken).collect();
    assert!(
        shared.is_empty(),
        "a reserved check name and an unparseable config gave up into the SAME overwatch \
         bucket {shared:?}. They need different remedies — rename a check vs repair the \
         file — so a reader of the ledger cannot tell which one to apply, and recurrence \
         correlates two unrelated problems as one systemic issue. \
         reserved={reserved:?} broken={broken:?}"
    );

    // The remaining pair. Disjointness is not transitive, so reserved-vs-broken
    // and untrusted-vs-broken (pinned by `the_two_refusal_kinds_do_not_share_a_bucket`)
    // together say nothing about this one.
    let shared: Vec<_> = reserved.intersection(&untrusted).collect();
    assert!(
        shared.is_empty(),
        "a reserved check name and an untrusted project gave up into the SAME overwatch \
         bucket {shared:?}; the remedies are a rename and `donegate trust`. \
         reserved={reserved:?} untrusted={untrusted:?}"
    );

    // WHICH slug belongs to WHICH cause — the literals, deliberately.
    //
    // The assertions above pin the PARTITION: three causes, three buckets, no
    // overlap. They cannot pin the MAPPING. Transposing the two adjacent string
    // literals in `refusal_kind_slug`'s match keeps every set disjoint and
    // every assertion above green, while sending an operator reading the ledger
    // to repair a file that parses fine and to rename a check that is named
    // fine. That transposition is the single cheapest mistake available in this
    // change, and nothing else in the crate would catch it: these slugs cross a
    // crate boundary (overwatch's recurrence bucketing, and a human reading
    // `overwatch violations`), so there is no compiler edge holding the two
    // sides together.
    //
    // These literals are therefore a cross-crate contract, not internal
    // naming, and they are MEANT to be expensive to change: renaming a slug
    // orphans every ledger entry already written under the old one and silently
    // restarts its recurrence count. A failing test is the cheapest way to be
    // told that is happening.
    //
    // Placed last on purpose: under a transposition the assertions above must
    // still pass, so reaching this point at all is the evidence that
    // disjointness alone does not cover the mapping.
    assert!(
        reserved.iter().any(|s| s.contains("reserved-check-name")),
        "the reserved-check-name cause must be recorded under its own slug; a reader of the \
         ledger uses this string to pick the remedy (rename the check). got: {reserved:?}"
    );
    assert!(
        broken.iter().any(|s| s.contains("unreadable-config")),
        "the unparseable-config cause must be recorded under its own slug; a reader of the \
         ledger uses this string to pick the remedy (repair the file). got: {broken:?}"
    );
}
