//! Spec `sitecraft-capabilities` R7 — the closed consistency rule table of
//! `sitecraft decide check` (rulings D7, C1, C3, U4).
//!
//! # Surface assumed (rulings D7)
//!
//! * `sitecraft --root <root> decide check` on an otherwise complete record
//!   (purpose recorded, every area settled, every active question answered):
//!   - V1..V5 firing → exit 1; the output (stdout+stderr) contains
//!     `<id>: <text>` (here: the substring `"<id>: "` followed by non-blank
//!     text) and the involved area ids, question ids and answers.
//!   - W1 alone → stderr contains `warning: W1: `, exit unchanged (0 here);
//!     with another pending item (no purpose) the exit is the same 1 as
//!     without W1.
//!   - Not firing → no `<id>: ` for that rule in the output.
//! * Terms referring to an unrecorded answer (area irrelevant, follow-up
//!   inactive) are false.
//! * Domain-question answers never change the rule verdict.
//! * The table lives in `crates/sitecraft/src/decide/`: the set of rule-id
//!   tokens (`\b[WV][0-9]+\b`) in the files there is exactly
//!   {W1, V1, V2, V3, V4, V5}.
//! * SKILL.md Phase 4 tells the LLM, after `done`, to re-read the answer list
//!   (domain questions included) and to confirm contradictions with
//!   AskUserQuestion (here: the Phase 4 section contains `読み直`, `食い違`,
//!   `AskUserQuestion` and `ドメイン` or `domain`).
//!
//! Records are written directly (canonical TOML form; `notification.channel`
//! as an array, rulings D8). Unlisted questions of a relevant area get their
//! first choice, chosen so no other rule fires.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod spec_common;

use std::collections::{BTreeMap, BTreeSet};

use sitecraft::decide::catalog;
use sitecraft::Determination;
use spec_common::*;

const RULES: [&str; 6] = ["W1", "V1", "V2", "V3", "V4", "V5"];

/// A complete record: `relevant` areas get `answers` (+ first-choice fill
/// for every other active question); every other area is irrelevant.
fn record(
    relevant: &[(&str, &[(&str, &str)])],
    with_purpose: bool,
    domain: Option<&str>,
) -> String {
    let all = match catalog::areas() {
        Determination::Known(a) => a,
        Determination::Undetermined(u) => panic!("catalog: {}", u.as_str()),
    };
    let mut s = String::new();
    if with_purpose {
        s.push_str(&purpose("Rule table spec test."));
    }
    for area in &all {
        let Some((_, given)) = relevant.iter().find(|(a, _)| *a == area.id) else {
            s.push_str(&format!(
                "[areas.{}]\nrelevance = \"irrelevant\"\n\n",
                area.id
            ));
            continue;
        };
        let mut answers: BTreeMap<String, String> = given
            .iter()
            .map(|(q, a)| (q.to_string(), a.to_string()))
            .collect();
        loop {
            let missing: Vec<_> = area
                .questions
                .iter()
                .filter(|q| catalog::is_active(q, &answers) && !answers.contains_key(&q.id))
                .collect();
            if missing.is_empty() {
                break;
            }
            for q in missing {
                let a = q
                    .choices
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "free text".to_string());
                answers.insert(q.id.clone(), a);
            }
        }
        s.push_str(&format!(
            "[areas.{}]\nrelevance = \"relevant\"\n\n[areas.{}.answers]\n",
            area.id, area.id
        ));
        for (q, a) in &answers {
            if area.id == "notification" && q == "channel" {
                s.push_str(&format!("{q} = [{}]\n", toml_str(a)));
            } else {
                s.push_str(&format!("{q} = {}\n", toml_str(a)));
            }
        }
        s.push('\n');
    }
    if let Some(ans) = domain {
        s.push_str(&format!(
            "[[questions]]\nid = \"launch-path\"\ntext = \"How is the job started?\"\nchoices = [\"foreground\", \"background\", \"never\"]\nanswer = {}\n\n",
            toml_str(ans)
        ));
    }
    s
}

fn check(body: &str) -> Out {
    let p = Project::new();
    p.write_decisions(body);
    p.run(&["decide", "check"])
}

/// Rule ids reported as `<id>: <non-blank>` in the output.
fn fired(o: &Out) -> BTreeSet<&'static str> {
    let all = o.all();
    RULES
        .iter()
        .copied()
        .filter(|id| {
            all.match_indices(&format!("{id}: ")).any(|(i, m)| {
                !all[i + m.len()..]
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim()
                    .is_empty()
            })
        })
        .collect()
}

#[track_caller]
fn expect_fires(body: &str, id: &str, involved: &[&str]) {
    let o = check(body);
    let f = fired(&o);
    assert_eq!(
        f,
        BTreeSet::from([RULES.iter().copied().find(|r| *r == id).unwrap()]),
        "exactly {id} must fire\n{}",
        o.describe()
    );
    if id == "W1" {
        expect_code(&o, 0, "W1 is a warning only");
        assert!(
            o.stderr.contains("warning: W1: "),
            "W1 must be printed to stderr as `warning: W1: …`\n{}",
            o.describe()
        );
    } else {
        expect_code(&o, 1, &format!("{id} is a violation"));
    }
    for term in involved {
        assert!(
            o.all().contains(term),
            "{id}: the output must list the involved `{term}`\n{}",
            o.describe()
        );
    }
}

#[track_caller]
fn expect_quiet(body: &str, what: &str) {
    let o = check(body);
    expect_code(&o, 0, what);
    assert!(
        fired(&o).is_empty(),
        "{what}: no rule may fire\n{}",
        o.describe()
    );
}

const AUTH_YES: (&str, &[(&str, &str)]) = ("auth", &[("needs-login", "yes")]);
const AUTH_NO: (&str, &[(&str, &str)]) = ("auth", &[("needs-login", "no")]);
const EXPO_SERVER: (&str, &[(&str, &str)]) = ("exposure", &[("hosting", "server")]);
const EXPO_STATIC: (&str, &[(&str, &str)]) = ("exposure", &[("hosting", "static")]);

#[test]
fn w1_fires_on_auth_relevant_with_exposure_irrelevant() {
    expect_fires(&record(&[AUTH_YES], true, None), "W1", &[]);
}

#[test]
fn w1_does_not_fire_when_exposure_is_relevant_or_auth_irrelevant() {
    expect_quiet(
        &record(&[AUTH_YES, EXPO_SERVER], true, None),
        "auth + exposure relevant",
    );
    expect_quiet(&record(&[], true, None), "auth and exposure irrelevant");
}

#[test]
fn w1_does_not_change_the_exit_code_of_a_pending_record() {
    let with_w1 = check(&record(&[AUTH_YES], false, None));
    let without = check(&record(&[AUTH_YES, EXPO_SERVER], false, None));
    expect_code(&without, 1, "no purpose, no W1");
    expect_code(&with_w1, 1, "no purpose, W1");
    assert!(
        with_w1.stderr.contains("warning: W1: "),
        "W1 must still be shown\n{}",
        with_w1.describe()
    );
}

#[test]
fn v1_fires_on_static_hosting_with_server_side_processing() {
    for exec in ["in-request", "background"] {
        expect_fires(
            &record(
                &[EXPO_STATIC, ("processing", &[("execution", exec)])],
                true,
                None,
            ),
            "V1",
            &[
                "exposure",
                "hosting",
                "static",
                "processing",
                "execution",
                exec,
            ],
        );
    }
}

#[test]
fn v1_does_not_fire_for_browser_processing_or_server_hosting() {
    expect_quiet(
        &record(
            &[EXPO_STATIC, ("processing", &[("execution", "browser")])],
            true,
            None,
        ),
        "static + browser processing",
    );
    expect_quiet(
        &record(
            &[EXPO_SERVER, ("processing", &[("execution", "in-request")])],
            true,
            None,
        ),
        "server hosting + in-request",
    );
    expect_quiet(
        &record(&[EXPO_STATIC], true, None),
        "static, processing irrelevant",
    );
}

#[test]
fn v2_fires_on_static_hosting_with_local_disk_files() {
    expect_fires(
        &record(
            &[
                EXPO_STATIC,
                (
                    "storage",
                    &[("data-kind", "files"), ("file-storage", "local-disk")],
                ),
            ],
            true,
            None,
        ),
        "V2",
        &[
            "exposure",
            "hosting",
            "static",
            "storage",
            "file-storage",
            "local-disk",
        ],
    );
}

#[test]
fn v2_does_not_fire_for_object_storage_or_an_inactive_follow_up() {
    expect_quiet(
        &record(
            &[
                EXPO_STATIC,
                (
                    "storage",
                    &[("data-kind", "files"), ("file-storage", "object-storage")],
                ),
            ],
            true,
            None,
        ),
        "static + object storage",
    );
    // file-storage is inactive for structured data: the term is false.
    expect_quiet(
        &record(
            &[EXPO_STATIC, ("storage", &[("data-kind", "structured")])],
            true,
            None,
        ),
        "static + structured data (file-storage not asked)",
    );
    expect_quiet(
        &record(
            &[
                EXPO_SERVER,
                (
                    "storage",
                    &[("data-kind", "files"), ("file-storage", "local-disk")],
                ),
            ],
            true,
            None,
        ),
        "server hosting + local disk",
    );
}

#[test]
fn v3_fires_on_several_users_without_login() {
    for model in ["same-rights", "multi-role"] {
        expect_fires(
            &record(
                &[EXPO_SERVER, ("users-roles", &[("user-model", model)])],
                true,
                None,
            ),
            "V3",
            &["users-roles", "user-model", model, "auth"],
        );
        expect_fires(
            &record(
                &[
                    AUTH_NO,
                    EXPO_SERVER,
                    ("users-roles", &[("user-model", model)]),
                ],
                true,
                None,
            ),
            "V3",
            &[
                "users-roles",
                "user-model",
                model,
                "auth",
                "needs-login",
                "no",
            ],
        );
    }
}

#[test]
fn v3_does_not_fire_for_a_single_user_or_with_login() {
    expect_quiet(
        &record(
            &[
                EXPO_SERVER,
                ("users-roles", &[("user-model", "single-user")]),
            ],
            true,
            None,
        ),
        "single user, auth irrelevant",
    );
    expect_quiet(
        &record(
            &[
                AUTH_YES,
                EXPO_SERVER,
                ("users-roles", &[("user-model", "multi-role")]),
            ],
            true,
            None,
        ),
        "multi-role with login",
    );
}

#[test]
fn v4_fires_on_an_admin_ui_without_login() {
    expect_fires(
        &record(
            &[EXPO_SERVER, ("admin", &[("admin-ui", "yes")])],
            true,
            None,
        ),
        "V4",
        &["admin", "admin-ui", "yes", "auth"],
    );
    expect_fires(
        &record(
            &[AUTH_NO, EXPO_SERVER, ("admin", &[("admin-ui", "yes")])],
            true,
            None,
        ),
        "V4",
        &["admin", "admin-ui", "auth", "needs-login", "no"],
    );
}

#[test]
fn v4_does_not_fire_without_an_admin_ui_or_with_login() {
    expect_quiet(
        &record(&[EXPO_SERVER, ("admin", &[("admin-ui", "no")])], true, None),
        "no admin UI, auth irrelevant",
    );
    expect_quiet(
        &record(
            &[AUTH_YES, EXPO_SERVER, ("admin", &[("admin-ui", "yes")])],
            true,
            None,
        ),
        "admin UI with login",
    );
}

#[test]
fn v5_fires_on_failure_notification_without_notifications() {
    for on_failure in ["notify", "both"] {
        expect_fires(
            &record(&[("workflow", &[("on-failure", on_failure)])], true, None),
            "V5",
            &["workflow", "on-failure", on_failure, "notification"],
        );
    }
}

#[test]
fn v5_does_not_fire_for_retry_or_with_notifications() {
    expect_quiet(
        &record(&[("workflow", &[("on-failure", "retry")])], true, None),
        "on-failure retry",
    );
    expect_quiet(
        &record(
            &[
                ("workflow", &[("on-failure", "notify")]),
                ("notification", &[("channel", "in-app")]),
            ],
            true,
            None,
        ),
        "notify with notifications relevant",
    );
    // workflow irrelevant: on-failure is not recorded, the term is false.
    expect_quiet(&record(&[], true, None), "workflow irrelevant");
}

#[test]
fn domain_answers_never_change_the_rule_verdict() {
    let firing = [
        EXPO_STATIC,
        ("processing", &[("execution", "in-request")][..]),
    ];
    let quiet = [
        EXPO_SERVER,
        ("processing", &[("execution", "background")][..]),
    ];
    for (what, areas, want) in [("firing", &firing[..], 1), ("quiet", &quiet[..], 0)] {
        let mut seen = Vec::new();
        for ans in ["foreground", "background", "never"] {
            let o = check(&record(areas, true, Some(ans)));
            expect_code(&o, want, &format!("{what} record, domain answer {ans}"));
            seen.push(fired(&o));
        }
        assert!(
            seen.windows(2).all(|w| w[0] == w[1]),
            "{what}: domain answers changed the fired rules: {seen:?}"
        );
    }
}

#[test]
fn the_rule_table_lives_in_src_decide_with_exactly_six_ids() {
    let dir = crate_dir().join("src/decide");
    let re = regex::Regex::new(r"\b[WV][0-9]+\b").unwrap();
    let mut ids = BTreeSet::new();
    for e in std::fs::read_dir(&dir).unwrap() {
        let path = e.unwrap().path();
        if path.extension().and_then(|x| x.to_str()) == Some("rs") {
            let text = std::fs::read_to_string(&path).unwrap();
            for m in re.find_iter(&text) {
                ids.insert(m.as_str().to_string());
            }
        }
    }
    let want: BTreeSet<String> = RULES.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        ids,
        want,
        "rule ids in {} must be exactly W1, V1..V5",
        dir.display()
    );
}

#[test]
fn skill_phase_4_rereads_the_answers_and_asks_about_contradictions() {
    let phase4 = section(&skill_text(), "### Phase 4");
    assert!(!phase4.is_empty(), "SKILL.md has no Phase 4 section");
    for needle in ["読み直", "食い違", "AskUserQuestion"] {
        assert!(phase4.contains(needle), "Phase 4 must contain `{needle}`");
    }
    assert!(
        phase4.contains("ドメイン") || phase4.contains("domain"),
        "Phase 4 re-reading must include the domain questions"
    );
}
