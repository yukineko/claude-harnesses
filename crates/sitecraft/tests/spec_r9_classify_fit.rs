//! Spec `sitecraft-capabilities` R9 — purpose fit of reused plugins in
//! `decide classify` (rulings D9).
//!
//! # CLI surface assumed (rulings D9)
//!
//! * `SITECRAFT_MARKETPLACE=<marketplace.json> sitecraft --root <root> decide
//!   classify --feature F --kind dedicated-harness [--harness P]
//!   [--confirm-fit yes|no]` prints one JSON object that, besides the
//!   existing `feature` / `kind` / `harness` / `existing`, has
//!   `description` (the marketplace entry's `description` string when
//!   `existing` is true, `null` otherwise) and `reuse`:
//!   `"unconfirmed"` (existing, no fit recorded), `"confirmed"` (fit yes),
//!   `"rejected"` (fit no), `"not-applicable"` (existing false, or
//!   `--kind simple`, whose JSON also carries `reuse`).
//! * `--confirm-fit yes|no` only when `existing` is true; it records
//!   `[features.F] fit = "yes"|"no"`. With `existing` false or `--kind
//!   simple` it is a failing exit and no `fit` is recorded.
//! * SKILL.md: shows `description` with AskUserQuestion and records only the
//!   user's answer with `--confirm-fit`; uses the existing plugin only when
//!   `confirmed`; does not read `unconfirmed` as permission; `rejected` goes
//!   the new-plugin route; the old wording is gone; the D9 boundary
//!   (simple: in the app's process, for its users — ジョブ実行・予算制御・
//!   通知送信・認証; dedicated-harness: judging Claude Code sessions / tool
//!   calls / file edits — hook・gate・検査ハーネス) is stated.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod spec_common;

use serde_json::Value;
use spec_common::*;

fn mp() -> std::ffi::OsString {
    os(spec_fixtures().join("marketplace.json"))
}

fn classify(p: &Project, args: &[&str]) -> Out {
    let mut a = vec!["decide", "classify"];
    a.extend_from_slice(args);
    p.run_env(&a, &[("SITECRAFT_MARKETPLACE", mp())])
}

fn json(o: &Out) -> Value {
    o.json_objects()
        .pop()
        .unwrap_or_else(|| panic!("classify printed no JSON\n{}", o.describe()))
}

fn fit(p: &Project, feature: &str) -> Option<String> {
    p.decisions_bytes()?;
    p.decisions()
        .get("features")
        .and_then(|f| f.get(feature))
        .and_then(|f| f.get("fit"))
        .and_then(|f| f.as_str())
        .map(str::to_string)
}

const AUTHKIT_DESCRIPTION: &str =
    "Login and session gate for sites built with sitecraft (fixture description).";

#[test]
fn existing_plugin_without_fit_is_unconfirmed_with_its_description() {
    let p = Project::new();
    p.write_decisions(&areas_all_irrelevant());
    let o = classify(
        &p,
        &[
            "--feature",
            "login",
            "--kind",
            "dedicated-harness",
            "--harness",
            "authkit",
        ],
    );
    expect_code(&o, 0, "classify login (authkit exists)");
    let v = json(&o);
    assert_eq!(v["existing"], true, "{v}");
    assert_eq!(v["description"], AUTHKIT_DESCRIPTION, "{v}");
    assert_eq!(v["reuse"], "unconfirmed", "{v}");
}

#[test]
fn confirm_fit_yes_is_confirmed_and_recorded() {
    let p = Project::new();
    p.write_decisions(&areas_all_irrelevant());
    let o = classify(
        &p,
        &[
            "--feature",
            "login",
            "--kind",
            "dedicated-harness",
            "--harness",
            "authkit",
            "--confirm-fit",
            "yes",
        ],
    );
    expect_code(&o, 0, "classify --confirm-fit yes");
    let v = json(&o);
    assert_eq!(v["reuse"], "confirmed", "{v}");
    assert_eq!(v["description"], AUTHKIT_DESCRIPTION, "{v}");
    assert_eq!(fit(&p, "login").as_deref(), Some("yes"));
}

#[test]
fn confirm_fit_no_is_rejected_and_recorded() {
    let p = Project::new();
    p.write_decisions(&areas_all_irrelevant());
    let o = classify(
        &p,
        &[
            "--feature",
            "login",
            "--kind",
            "dedicated-harness",
            "--harness",
            "authkit",
            "--confirm-fit",
            "no",
        ],
    );
    expect_code(&o, 0, "classify --confirm-fit no");
    assert_eq!(json(&o)["reuse"], "rejected");
    assert_eq!(fit(&p, "login").as_deref(), Some("no"));
}

#[test]
fn missing_plugin_is_not_applicable_with_null_description() {
    let p = Project::new();
    p.write_decisions(&areas_all_irrelevant());
    let o = classify(
        &p,
        &[
            "--feature",
            "chat",
            "--kind",
            "dedicated-harness",
            "--harness",
            "chatkit",
        ],
    );
    expect_code(&o, 0, "classify chat (no such plugin)");
    let v = json(&o);
    assert_eq!(v["existing"], false, "{v}");
    assert!(
        v.get("description").is_some_and(Value::is_null),
        "description must be present and null: {v}"
    );
    assert_eq!(v["reuse"], "not-applicable", "{v}");
}

#[test]
fn simple_feature_is_not_applicable() {
    let p = Project::new();
    p.write_decisions(&areas_all_irrelevant());
    let o = classify(&p, &["--feature", "about-page", "--kind", "simple"]);
    expect_code(&o, 0, "classify simple");
    assert_eq!(json(&o)["reuse"], "not-applicable");
}

#[test]
fn confirm_fit_is_refused_when_nothing_exists_to_reuse() {
    let p = Project::new();
    p.write_decisions(&areas_all_irrelevant());
    let o = classify(
        &p,
        &[
            "--feature",
            "chat",
            "--kind",
            "dedicated-harness",
            "--harness",
            "chatkit",
            "--confirm-fit",
            "yes",
        ],
    );
    expect_failure(&o, "--confirm-fit with existing = false");
    assert_eq!(fit(&p, "chat"), None, "no fit may be recorded");

    let o = classify(
        &p,
        &[
            "--feature",
            "about-page",
            "--kind",
            "simple",
            "--confirm-fit",
            "no",
        ],
    );
    expect_failure(&o, "--confirm-fit with --kind simple");
    assert_eq!(fit(&p, "about-page"), None, "no fit may be recorded");

    // Positive control: the flag is accepted for an existing plugin.
    let o = classify(
        &p,
        &[
            "--feature",
            "login",
            "--kind",
            "dedicated-harness",
            "--harness",
            "authkit",
            "--confirm-fit",
            "yes",
        ],
    );
    expect_code(&o, 0, "--confirm-fit with existing = true");
}

#[test]
fn skill_asks_the_user_about_fit_and_reuses_only_confirmed() {
    let s = skill_text();
    assert_paragraph_with(
        &s,
        &["description", "AskUserQuestion", "--confirm-fit"],
        "R9: show description with AskUserQuestion, record the answer with --confirm-fit",
    );
    for word in ["confirmed", "unconfirmed", "rejected"] {
        assert!(s.contains(word), "SKILL.md must cover reuse = {word}");
    }
    // The three reuse rules themselves (rulings D9), each in one paragraph.
    assert_paragraph_with(
        &s,
        &["confirmed", "のときだけ", "既存"],
        "R9: the existing plugin is used only when reuse = confirmed",
    );
    assert_paragraph_with(
        &s,
        &["unconfirmed", "再利用"],
        "R9: unconfirmed is not read as permission to reuse",
    );
    assert_paragraph_with(
        &s,
        &["rejected", "新規", "別名"],
        "R9: rejected takes the new-plugin (another name) route",
    );
    assert!(
        !s.contains("existing: true ならその plugin を使う"),
        "the old wording must be gone"
    );
    assert!(
        !s.contains("`existing: true` — 既存の専用 plugin がある。その plugin を使う"),
        "the old Phase 5 line (existing: true → use the plugin) must be gone"
    );
}

#[test]
fn skill_states_the_simple_dedicated_harness_boundary() {
    let s = skill_text();
    assert_paragraph_with(
        &s,
        &["simple", "ジョブ実行", "予算制御", "通知送信", "認証"],
        "R9: app-process features for its users are simple",
    );
    assert_paragraph_with(
        &s,
        &[
            "dedicated-harness",
            "Claude Code",
            "hook",
            "gate",
            "検査ハーネス",
        ],
        "R9: only judges of Claude Code sessions / tool calls / file edits are dedicated-harness",
    );
}
