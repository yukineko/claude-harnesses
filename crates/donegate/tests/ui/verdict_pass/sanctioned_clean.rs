//! Positive control: the sanctioned `harness_core::verdict` paths that
//! `donegate::gate` actually calls MUST still compile.
//!
//! `donegate` is a bin-only crate (no `[lib]` target, no `src/lib.rs`), so an
//! external integration test cannot `use donegate::gate::...` the way
//! `blastguard`'s equivalent positive control reaches into
//! `blastguard::diffrisk::SensitiveConfig`. This is therefore NOT a copy of
//! blastguard's control: gate.rs never calls `Verdict::adjudicate`,
//! `Verdict::violation`, `Verdict::undetermined`, or `Determination::map` —
//! only the calls enumerated below, each traced to its exact line in
//! `crates/donegate/src/gate.rs` as of this writing, and reproduced here in
//! the same shape against the same types:
//!
//! * `gate.rs:21` — `pub type ChangeScope = Determination<Option<Vec<String>>>;`
//!   (mirrored below as the local `ChangeScope` alias).
//! * `gate.rs:117` (`scan_scope`, `Files(v)` arm) —
//!   `Determination::known(Some(v))`.
//! * `gate.rs:118` (`scan_scope`, `NotRepo` arm) — `Determination::known(None)`.
//! * `gate.rs:119` (`scan_scope`, `Failed` arm) —
//!   `Determination::undetermined("git could not report the changed files …")`.
//! * `gate.rs:136` (`evaluate`) — `scope.clone().require()`, resolved with a
//!   `match Required::Determined(..) / Required::Blocked(undetermined)` and
//!   `undetermined.blocks()`.
//! * `gate.rs:231,234,239` (`human_report`) — matching
//!   `Determination::Known(None)` / `Determination::Undetermined(why)` /
//!   `Determination::Known(Some(_))` directly, and formatting `why` via its
//!   `Display` impl (gate.rs:233: `format!("… {why} …")`).
//! * `gate.rs:62-69` (`GateReport::verdict`) — `Verdict::from_findings` fed a
//!   `Vec<Reason>` built with `Reason::new(format!(...))`.
//! * `gate.rs:72` (`GateReport::all_green`) — `!verdict.blocks()`.
//! * `gate.rs:593,603` (`gate.rs` tests) — matching `Verdict::Violation(_)` /
//!   `Verdict::Clean(_)` directly.
//!
//! Proving these compile shows the contract does not over-block the paths
//! donegate's own migration onto `harness_core::verdict` depends on.

use harness_core::verdict::{Determination, Reason, Required, Verdict};

/// Mirrors `donegate::gate::ChangeScope` (gate.rs:21) verbatim.
type ChangeScope = Determination<Option<Vec<String>>>;

/// Mirrors `donegate::gate::scan_scope` (gate.rs:115-123): the three
/// `crate::git::ChangeScan` arms (`Files`/`NotRepo`/`Failed`) collapse onto
/// exactly these three `Determination` constructors.
fn scan_scope_like(files: Option<Vec<String>>, undetermined: bool) -> ChangeScope {
    if undetermined {
        // gate.rs:119
        Determination::undetermined(
            "git could not report the changed files (unspawnable, or a sub-command exited non-zero)",
        )
    } else {
        // gate.rs:117 / gate.rs:118 (Some(v) for Files, None for NotRepo)
        Determination::known(files)
    }
}

fn main() {
    let files_scope: ChangeScope = scan_scope_like(Some(vec!["a.md".to_string()]), false);
    let no_repo_scope: ChangeScope = scan_scope_like(None, false);
    let failed_scope: ChangeScope = scan_scope_like(None, true);

    // gate.rs:231,234,239 (human_report) — match the variants directly, and
    // format the reason via Display the way gate.rs:233 does.
    for scope in [&files_scope, &no_repo_scope, &failed_scope] {
        match scope {
            Determination::Known(None) => {}
            Determination::Undetermined(why) => {
                let _ = format!("(git state undetermined: {why} — all checks ran unscoped)");
            }
            Determination::Known(Some(_)) => {}
        }
    }

    // gate.rs:136 (evaluate) — scope.clone().require().
    let changed: Option<Vec<String>> = match files_scope.clone().require() {
        Required::Determined(files) => files,
        Required::Blocked(undetermined) => {
            assert!(undetermined.blocks(), "an undetermined scope must block");
            None
        }
    };
    assert_eq!(changed, Some(vec!["a.md".to_string()]));

    let scope_verdict = match failed_scope.require() {
        Required::Determined(v) => panic!("expected Blocked, got Determined({v:?})"),
        Required::Blocked(v) => v,
    };
    assert!(matches!(scope_verdict, Verdict::Undetermined(_)));

    // gate.rs:62-69 (GateReport::verdict) — Verdict::from_findings fed from
    // Reason::new'd strings built the same way (`"{name} ({status})"`).
    let clean: Verdict = Verdict::from_findings(vec![]);
    let violation: Verdict = Verdict::from_findings(vec![Reason::new(format!(
        "{} ({})",
        "check-a", "failed, exit 1"
    ))]);

    // gate.rs:72 (GateReport::all_green) — !verdict.blocks().
    assert!(!clean.blocks());
    assert!(violation.blocks());

    // gate.rs:593,603 (gate.rs tests) — match the variants directly.
    assert!(matches!(violation, Verdict::Violation(_)));
    assert!(matches!(clean, Verdict::Clean(_)));
}
