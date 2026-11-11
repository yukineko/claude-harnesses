//! GitHub #1637 reproduction: library consumers (condukt gate_exec/verify,
//! specguard forge, daily) call `detect::detect`, which uses `SafeRoots::none()`.
//! The scratch-place rule only applies when a location model (resolver) exists
//! (`SafeRoots::has_location_model`), so the same `git clone` outside every
//! managed place is Ask for the hook binary and Allow for the library entry.

use blastguard::detect::{detect, detect_scoped};
use blastguard::model::Decision;
use blastguard::scope::SafeRoots;
use serde_json::json;

fn identity(p: &str) -> Option<String> {
    Some(p.to_string())
}

fn model() -> SafeRoots {
    SafeRoots::new(
        Some("/Users/u/proj"),
        Some("/Users/u/proj"),
        Some("/Users/u"),
        Some("/tmp"),
        Some(identity),
    )
}

fn bash(cmd: &str) -> serde_json::Value {
    json!({ "command": cmd })
}

const OUTSIDE: &str = "git clone https://example.invalid/r.git /Users/u/Desktop/clone";
const INSIDE: &str = "git clone https://example.invalid/r.git /tmp/clone";

/// CONTROL (passes): with a location model, a clone outside managed places is not Allow.
#[test]
fn control_scoped_model_refuses_clone_outside_managed_places() {
    let d = detect_scoped("Bash", Some(&bash(OUTSIDE)), &model());
    assert!(!matches!(d, Decision::Allow), "control: expected Ask/Deny, got {d:?}");
}

/// CONTROL (passes): a clone inside a managed place is Allow under both entries.
#[test]
fn control_clone_inside_managed_place_allowed_both_entries() {
    assert!(matches!(detect_scoped("Bash", Some(&bash(INSIDE)), &model()), Decision::Allow));
    assert!(matches!(detect("Bash", Some(&bash(INSIDE))), Decision::Allow));
}

/// RED: the library entry must not be weaker than the scoped one for the same command.
#[test]
fn library_detect_applies_scratch_place_rule_like_scoped() {
    let d = detect("Bash", Some(&bash(OUTSIDE)));
    assert!(
        !matches!(d, Decision::Allow),
        "library detect() allowed `{OUTSIDE}` while detect_scoped refuses it"
    );
}
