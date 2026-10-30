//! End-to-end tests of `sitecraft decide next|record|check|add-question|classify`
//! against the real binary in a temp project root.
//!
//! User rulings pinned here:
//! - every one of the 12 areas needs an explicit relevant/irrelevant decision;
//!   `irrelevant` is recorded and settles the area (no questions), never a
//!   silent skip;
//! - unanswered never passes and there are no defaults (absent record = exit 2,
//!   pending = exit 1, unreadable/invalid = exit 2);
//! - LLM-added domain questions count until answered;
//! - existing dedicated-harness plugins are detected from `marketplace.json`.
//!
//! Every exit-2 assertion is paired with a positive control in the same root so
//! a binary that always exits 2 cannot pass it.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;
use tempfile::TempDir;

const AREA_IDS: [&str; 12] = [
    "auth",
    "users-roles",
    "admin",
    "storage",
    "data-analytics",
    "web-analytics",
    "processing",
    "workflow",
    "notification",
    "ai",
    "formats",
    "exposure",
];

/// Bound on `decide next` / `record` round trips when walking to `done`.
const MAX_STEPS: usize = 300;

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Out {
    fn all(&self) -> String {
        format!("{}\n{}", self.stdout, self.stderr)
    }
    fn json(&self) -> Value {
        let line = self
            .stdout
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or_else(|| panic!("no stdout; stderr: {}", self.stderr));
        serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {}", self.stdout))
    }
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/decide")
        .join(name)
}

fn run_env(root: &Path, args: &[&str], marketplace: Option<&Path>) -> Out {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sitecraft"));
    cmd.current_dir(root)
        .arg("--root")
        .arg(root)
        .arg("decide")
        .args(args)
        .env("SITECRAFT_STATE_DIR", root.join(".state"))
        .env_remove("SITECRAFT_MARKETPLACE");
    if let Some(m) = marketplace {
        cmd.env("SITECRAFT_MARKETPLACE", m);
    }
    let o = cmd.output().expect("spawn sitecraft");
    Out {
        code: o.status.code().expect("sitecraft killed by a signal"),
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
    }
}

fn run(root: &Path, args: &[&str]) -> Out {
    run_env(root, args, None)
}

fn record_path(root: &Path) -> PathBuf {
    root.join("sitecraft").join("decisions.toml")
}

fn write_record(root: &Path, body: &str) {
    std::fs::create_dir_all(root.join("sitecraft")).unwrap();
    std::fs::write(record_path(root), body).unwrap();
}

/// TOML for: every area in `irrelevant` judged irrelevant, nothing else.
fn irrelevant_toml(irrelevant: &[&str]) -> String {
    irrelevant
        .iter()
        .map(|a| format!("[areas.{a}]\nrelevance = \"irrelevant\"\n\n"))
        .collect()
}

fn all_irrelevant_toml() -> String {
    irrelevant_toml(&AREA_IDS)
}

fn check(root: &Path) -> Out {
    run(root, &["check"])
}

fn next(root: &Path) -> Out {
    run(root, &["next"])
}

fn record(root: &Path, area: &str, q: &str, a: &str) -> Out {
    run(
        root,
        &["record", "--area", area, "--question", q, "--answer", a],
    )
}

fn expect_code(o: &Out, want: i32, what: &str) {
    assert_eq!(
        o.code, want,
        "{what}: expected exit {want}, got {}\nstdout: {}\nstderr: {}",
        o.code, o.stdout, o.stderr
    );
}

/// `decide next` → `(area, question_id, text, choices)`, or `None` for done.
fn next_q(root: &Path) -> Option<(String, String, String, Vec<String>)> {
    let o = next(root);
    expect_code(&o, 0, "decide next");
    let v = o.json();
    match v["status"].as_str() {
        Some("done") => None,
        Some("ask") => {
            let s = |k: &str| {
                v[k].as_str()
                    .unwrap_or_else(|| panic!("next JSON lacks string `{k}`: {v}"))
                    .to_string()
            };
            let choices = v["choices"]
                .as_array()
                .unwrap_or_else(|| panic!("next JSON lacks array `choices`: {v}"))
                .iter()
                .map(|c| c.as_str().expect("choice is a string").to_string())
                .collect();
            Some((s("area"), s("question_id"), s("text"), choices))
        }
        other => panic!("next JSON status must be ask|done, got {other:?}: {v}"),
    }
}

/// Answer `decide next` until done. `relevance(area)` picks the relevance
/// answer; other questions get their first choice (or free text). Returns
/// every `(area, question_id)` asked. Panics if a question repeats after it
/// was recorded or the walk does not terminate.
fn walk_to_done(root: &Path, relevance: impl Fn(&str) -> &'static str) -> Vec<(String, String)> {
    let mut asked = Vec::new();
    for _ in 0..MAX_STEPS {
        let Some((area, qid, text, choices)) = next_q(root) else {
            return asked;
        };
        assert!(!text.trim().is_empty(), "{area}/{qid} has empty text");
        let key = (area.clone(), qid.clone());
        assert!(
            !asked.contains(&key),
            "{area}/{qid} asked again after being recorded"
        );
        let answer = if qid == "relevance" {
            relevance(&area).to_string()
        } else if let Some(c) = choices.first() {
            c.clone()
        } else {
            "free text answer".to_string()
        };
        let o = record(root, &area, &qid, &answer);
        expect_code(&o, 0, &format!("record {area}/{qid}={answer}"));
        asked.push(key);
    }
    panic!("decide next did not reach done within {MAX_STEPS} steps: {asked:?}");
}

// ---------------------------------------------------------------- check

#[test]
fn check_without_record_is_undetermined_then_complete_record_is_clean() {
    let t = TempDir::new().unwrap();
    let root = t.path();
    assert!(!record_path(root).exists());
    expect_code(&check(root), 2, "check with no decisions.toml");
    // Control: the same root with every area explicitly judged is clean.
    write_record(root, &all_irrelevant_toml());
    expect_code(&check(root), 0, "check with all 12 areas judged irrelevant");
}

#[test]
fn check_names_every_area_without_a_relevance_decision() {
    let t = TempDir::new().unwrap();
    let root = t.path();
    let judged = ["auth", "storage", "ai"];
    write_record(root, &irrelevant_toml(&judged));
    let o = check(root);
    expect_code(&o, 1, "check with 9 areas unjudged");
    let all = o.all();
    for a in AREA_IDS.iter().filter(|a| !judged.contains(a)) {
        assert!(all.contains(a), "pending area `{a}` not named in:\n{all}");
    }
}

#[test]
fn irrelevant_counts_as_decided_and_one_missing_area_still_blocks() {
    let t = TempDir::new().unwrap();
    let root = t.path();
    // 11 judged irrelevant, `exposure` never judged: must not pass by default.
    let eleven: Vec<&str> = AREA_IDS
        .iter()
        .copied()
        .filter(|a| *a != "exposure")
        .collect();
    write_record(root, &irrelevant_toml(&eleven));
    let o = check(root);
    expect_code(&o, 1, "check with exposure unjudged");
    assert!(
        o.all().contains("exposure"),
        "pending `exposure` not named:\n{}",
        o.all()
    );
    // Judging it irrelevant (recorded, not skipped) settles it.
    expect_code(
        &record(root, "exposure", "relevance", "irrelevant"),
        0,
        "record exposure irrelevant",
    );
    expect_code(&check(root), 0, "check after all 12 judged");
    let body = std::fs::read_to_string(record_path(root)).unwrap();
    let v: toml::Value = toml::from_str(&body).unwrap();
    assert_eq!(
        v["areas"]["exposure"]["relevance"].as_str(),
        Some("irrelevant"),
        "irrelevant must be recorded in decisions.toml:\n{body}"
    );
}

#[test]
fn relevant_area_with_unanswered_questions_blocks_until_answered() {
    let t = TempDir::new().unwrap();
    let root = t.path();
    let others: Vec<&str> = AREA_IDS.iter().copied().filter(|a| *a != "auth").collect();
    write_record(root, &irrelevant_toml(&others));
    expect_code(
        &record(root, "auth", "relevance", "relevant"),
        0,
        "record auth relevant",
    );
    let o = check(root);
    expect_code(&o, 1, "relevant auth with no answers (no defaults)");
    assert!(
        o.all().contains("auth"),
        "pending `auth` not named:\n{}",
        o.all()
    );
    let asked = walk_to_done(root, |_| "irrelevant");
    assert!(
        asked.iter().any(|(a, q)| a == "auth" && q != "relevance"),
        "relevant auth must ask its questions: {asked:?}"
    );
    expect_code(&check(root), 0, "check after answering auth");
}

// ---------------------------------------------------------------- next / record

#[test]
fn next_on_absent_record_asks_a_relevance_question() {
    let t = TempDir::new().unwrap();
    let root = t.path();
    let (area, qid, _text, choices) = next_q(root).expect("absent record cannot be done");
    assert!(AREA_IDS.contains(&area.as_str()), "unknown area {area}");
    assert_eq!(
        qid, "relevance",
        "an unjudged area is asked its relevance first"
    );
    let set: BTreeSet<&str> = choices.iter().map(String::as_str).collect();
    assert!(
        set.contains("relevant") && set.contains("irrelevant"),
        "relevance choices: {choices:?}"
    );
    // `next` is read-only: it must not fabricate a record.
    let again = next_q(root).unwrap();
    assert_eq!((again.0, again.1), (area, qid));
}

#[test]
fn next_json_has_the_documented_shape() {
    let t = TempDir::new().unwrap();
    let o = next(t.path());
    expect_code(&o, 0, "decide next");
    let v = o.json();
    assert_eq!(v["status"], "ask", "{v}");
    for k in ["area", "question_id", "text"] {
        assert!(v[k].is_string(), "`{k}` must be a string: {v}");
    }
    assert!(v["choices"].is_array(), "`choices` must be an array: {v}");
}

#[test]
fn every_area_is_asked_its_relevance_and_all_irrelevant_reaches_done() {
    let t = TempDir::new().unwrap();
    let root = t.path();
    let asked = walk_to_done(root, |_| "irrelevant");
    let areas: BTreeSet<&str> = asked.iter().map(|(a, _)| a.as_str()).collect();
    let want: BTreeSet<&str> = AREA_IDS.into_iter().collect();
    assert_eq!(areas, want, "every area needs an explicit decision");
    assert!(
        asked.iter().all(|(_, q)| q == "relevance"),
        "irrelevant areas must not be asked further questions: {asked:?}"
    );
    let o = next(root);
    expect_code(&o, 0, "next when done");
    assert_eq!(o.json()["status"], "done");
    expect_code(&check(root), 0, "check when done");
}

#[test]
fn irrelevant_area_is_settled_and_not_asked_again() {
    let t = TempDir::new().unwrap();
    let root = t.path();
    expect_code(
        &record(root, "auth", "relevance", "irrelevant"),
        0,
        "record auth irrelevant",
    );
    let asked = walk_to_done(root, |_| "irrelevant");
    assert!(
        asked.iter().all(|(a, _)| a != "auth"),
        "auth was judged irrelevant but asked again: {asked:?}"
    );
    assert_eq!(asked.len(), 11);
}

#[test]
fn recorded_answer_is_not_returned_by_next() {
    let t = TempDir::new().unwrap();
    let root = t.path();
    let others: Vec<&str> = AREA_IDS.iter().copied().filter(|a| *a != "auth").collect();
    write_record(root, &irrelevant_toml(&others));
    let (area, qid, ..) = next_q(root).unwrap();
    assert_eq!((area.as_str(), qid.as_str()), ("auth", "relevance"));
    expect_code(
        &record(root, "auth", "relevance", "relevant"),
        0,
        "record relevant",
    );
    let (area2, qid2, _, choices) = next_q(root).unwrap();
    assert_eq!(area2, "auth", "relevant auth must now ask its own question");
    assert_ne!(qid2, "relevance");
    let ans = choices.first().cloned().unwrap_or_else(|| "x".into());
    expect_code(&record(root, "auth", &qid2, &ans), 0, "record auth answer");
    if let Some((a3, q3, ..)) = next_q(root) {
        assert!(
            !(a3 == "auth" && q3 == qid2),
            "next returned the already-answered {a3}/{q3}"
        );
    }
}

#[test]
fn auth_yes_asks_the_follow_up_and_no_does_not() {
    let mk = || {
        let t = TempDir::new().unwrap();
        let others: Vec<&str> = AREA_IDS.iter().copied().filter(|a| *a != "auth").collect();
        write_record(t.path(), &irrelevant_toml(&others));
        expect_code(
            &record(t.path(), "auth", "relevance", "relevant"),
            0,
            "auth relevant",
        );
        t
    };
    // Walk auth until its yes/no representative comes up; other auth
    // questions get their first choice.
    let ty = mk();
    let tn = mk();
    let mut gate = None;
    let mut seen = Vec::new();
    for _ in 0..MAX_STEPS {
        let (a, q, _, choices) = next_q(ty.path()).expect("auth yes/no question never asked");
        assert_eq!(
            a, "auth",
            "auth questions exhausted without a yes/no gate: {seen:?}"
        );
        if choices.iter().any(|c| c == "yes") && choices.iter().any(|c| c == "no") {
            gate = Some(q);
            break;
        }
        let ans = choices.first().cloned().unwrap_or_else(|| "x".into());
        expect_code(
            &record(ty.path(), "auth", &q, &ans),
            0,
            "auth pre-gate answer",
        );
        expect_code(
            &record(tn.path(), "auth", &q, &ans),
            0,
            "auth pre-gate answer (no run)",
        );
        seen.push(q);
    }
    let gate = gate.expect("auth yes/no gate");
    expect_code(&record(ty.path(), "auth", &gate, "yes"), 0, "auth gate yes");
    let after_yes = next_q(ty.path());
    let (a, q, ..) = after_yes.expect("yes must leave the auth-method follow-up");
    assert_eq!(a, "auth", "auth=yes must ask a follow-up in auth");
    assert_ne!(q, gate);
    let follow_up = q;

    expect_code(&record(tn.path(), "auth", &gate, "no"), 0, "auth gate no");
    let asked = walk_to_done(tn.path(), |_| "irrelevant");
    assert!(
        asked.iter().all(|(a, q)| !(a == "auth" && *q == follow_up)),
        "auth=no must not ask {follow_up}: {asked:?}"
    );
    expect_code(&check(tn.path()), 0, "auth=no settled");
    // And with yes, check blocks until the follow-up is answered.
    let o = check(ty.path());
    expect_code(&o, 1, "auth=yes with follow-up unanswered");
}

#[test]
fn record_rejects_answers_outside_the_choices_and_leaves_record_unchanged() {
    let t = TempDir::new().unwrap();
    let root = t.path();
    // Control: a valid relevance answer records.
    expect_code(
        &record(root, "auth", "relevance", "relevant"),
        0,
        "valid record",
    );
    let before = std::fs::read_to_string(record_path(root)).unwrap();
    let bad = record(root, "storage", "relevance", "maybe");
    expect_code(&bad, 2, "relevance=maybe");
    let unknown = record(root, "no-such-area", "relevance", "relevant");
    expect_code(&unknown, 2, "unknown area");
    assert_eq!(std::fs::read_to_string(record_path(root)).unwrap(), before);
}

// ---------------------------------------------------------------- domain questions

#[test]
fn added_domain_question_blocks_check_until_answered() {
    let t = TempDir::new().unwrap();
    let root = t.path();
    write_record(root, &all_irrelevant_toml());
    expect_code(&check(root), 0, "control: all areas judged");
    let o = run(
        root,
        &[
            "add-question",
            "--id",
            "vrm-other-formats",
            "--text",
            "How should formats other than VRM be handled?",
            "--choice",
            "reject",
            "--choice",
            "convert",
        ],
    );
    expect_code(&o, 0, "add-question");
    let c = check(root);
    expect_code(&c, 1, "unanswered domain question");
    assert!(
        c.all().contains("vrm-other-formats"),
        "pending question not named:\n{}",
        c.all()
    );

    let (area, qid, text, choices) = next_q(root).expect("domain question pending");
    assert_eq!(area, "domain");
    assert_eq!(qid, "vrm-other-formats");
    assert!(text.contains("VRM"));
    assert_eq!(choices, vec!["reject".to_string(), "convert".to_string()]);

    expect_code(
        &record(root, "domain", "vrm-other-formats", "accept"),
        2,
        "answer outside choices",
    );
    expect_code(&check(root), 1, "still unanswered after rejected record");
    expect_code(
        &record(root, "domain", "vrm-other-formats", "convert"),
        0,
        "valid answer",
    );
    expect_code(&check(root), 0, "domain question answered");
    assert!(next_q(root).is_none(), "everything answered = done");
}

#[test]
fn free_text_domain_question_accepts_any_answer() {
    let t = TempDir::new().unwrap();
    let root = t.path();
    write_record(root, &all_irrelevant_toml());
    expect_code(
        &run(
            root,
            &["add-question", "--id", "brand", "--text", "Brand colour?"],
        ),
        0,
        "add free-text question",
    );
    expect_code(&check(root), 1, "free-text unanswered");
    let (_, _, _, choices) = next_q(root).unwrap();
    assert!(choices.is_empty());
    expect_code(&record(root, "domain", "brand", "teal"), 0, "free text");
    expect_code(&check(root), 0, "answered");
}

#[test]
fn duplicate_domain_question_is_rejected() {
    let t = TempDir::new().unwrap();
    let root = t.path();
    let add = |id: &str| run(root, &["add-question", "--id", id, "--text", "q?"]);
    expect_code(&add("dup"), 0, "first add");
    expect_code(&add("dup"), 2, "duplicate add");
}

// ---------------------------------------------------------------- broken records

fn assert_invalid_record_is_undetermined(body: &str, what: &str) {
    let t = TempDir::new().unwrap();
    let root = t.path();
    // Control: a valid record in the same root is decidable (not 2).
    write_record(root, &all_irrelevant_toml());
    expect_code(&check(root), 0, &format!("{what}: control check"));
    expect_code(&next(root), 0, &format!("{what}: control next"));
    write_record(root, body);
    expect_code(&check(root), 2, &format!("{what}: check"));
    expect_code(&next(root), 2, &format!("{what}: next"));
}

#[test]
fn broken_toml_is_undetermined_for_check_and_next() {
    assert_invalid_record_is_undetermined("[areas.auth\nrelevance = ", "broken TOML");
}

#[test]
fn unknown_area_id_is_undetermined_for_check_and_next() {
    let body = format!(
        "{}[areas.payments]\nrelevance = \"irrelevant\"\n",
        all_irrelevant_toml()
    );
    assert_invalid_record_is_undetermined(&body, "unknown area id");
}

#[test]
fn invalid_relevance_value_is_undetermined() {
    let body = all_irrelevant_toml().replacen("\"irrelevant\"", "\"maybe\"", 1);
    assert_invalid_record_is_undetermined(&body, "relevance = maybe");
}

#[test]
fn domain_answer_outside_choices_is_undetermined_for_check_and_next() {
    let body = format!(
        "{}[[questions]]\nid = \"q\"\ntext = \"t?\"\nchoices = [\"a\", \"b\"]\nanswer = \"zzz\"\n",
        all_irrelevant_toml()
    );
    assert_invalid_record_is_undetermined(&body, "domain answer outside choices");
}

#[test]
fn catalog_answer_outside_choices_is_undetermined_for_check_and_next() {
    // Discover the auth gate question (with choices) through the CLI.
    let t = TempDir::new().unwrap();
    let others: Vec<&str> = AREA_IDS.iter().copied().filter(|a| *a != "auth").collect();
    write_record(t.path(), &irrelevant_toml(&others));
    expect_code(
        &record(t.path(), "auth", "relevance", "relevant"),
        0,
        "auth relevant",
    );
    let (_, gate, _, choices) = next_q(t.path()).unwrap();
    assert!(!choices.is_empty(), "auth gate must have choices");
    let body = format!(
        "{}[areas.auth]\nrelevance = \"relevant\"\n[areas.auth.answers]\n\"{gate}\" = \"not-a-choice\"\n",
        irrelevant_toml(&others)
    );
    assert_invalid_record_is_undetermined(&body, "catalog answer outside choices");
}

#[test]
fn unknown_top_level_key_is_undetermined() {
    let body = format!("{}default = \"yes\"\n", all_irrelevant_toml());
    // `default` after a table header lands inside that table: still unknown.
    assert_invalid_record_is_undetermined(&body, "unknown key");
}

// ---------------------------------------------------------------- classify

fn classify(root: &Path, args: &[&str], mp: Option<&Path>) -> Out {
    let mut a = vec!["classify"];
    a.extend_from_slice(args);
    run_env(root, &a, mp)
}

fn recorded_features(root: &Path) -> toml::Value {
    let body = std::fs::read_to_string(record_path(root)).unwrap();
    let v: toml::Value = toml::from_str(&body).unwrap();
    v.get("features")
        .cloned()
        .unwrap_or(toml::Value::Table(Default::default()))
}

#[test]
fn classify_dedicated_harness_detects_existing_plugin() {
    let t = TempDir::new().unwrap();
    let root = t.path();
    write_record(root, &all_irrelevant_toml());
    let mp = fixture("marketplace.json");
    let o = classify(
        root,
        &[
            "--feature",
            "login",
            "--kind",
            "dedicated-harness",
            "--harness",
            "authkit",
        ],
        Some(&mp),
    );
    expect_code(&o, 0, "classify login");
    let v = o.json();
    assert_eq!(v["feature"], "login");
    assert_eq!(v["kind"], "dedicated-harness");
    assert_eq!(v["harness"], "authkit");
    assert_eq!(
        v["existing"], true,
        "authkit is in the fixture marketplace: {v}"
    );
    let f = recorded_features(root);
    assert_eq!(f["login"]["kind"].as_str(), Some("dedicated-harness"));
    assert_eq!(f["login"]["harness"].as_str(), Some("authkit"));
}

#[test]
fn classify_harness_defaults_to_feature_name() {
    let t = TempDir::new().unwrap();
    let mp = fixture("marketplace.json");
    let o = classify(
        t.path(),
        &["--feature", "vrmforge", "--kind", "dedicated-harness"],
        Some(&mp),
    );
    expect_code(&o, 0, "classify vrmforge");
    assert_eq!(o.json()["existing"], true, "{}", o.stdout);
}

#[test]
fn classify_dedicated_harness_not_in_marketplace_is_not_existing() {
    let t = TempDir::new().unwrap();
    let mp = fixture("marketplace.json");
    let o = classify(
        t.path(),
        &[
            "--feature",
            "chat",
            "--kind",
            "dedicated-harness",
            "--harness",
            "chatkit",
        ],
        Some(&mp),
    );
    expect_code(&o, 0, "classify chat");
    let v = o.json();
    assert_eq!(v["existing"], false, "{v}");
    assert_eq!(v["harness"], "chatkit");
}

#[test]
fn classify_simple_needs_no_marketplace() {
    let t = TempDir::new().unwrap();
    let root = t.path();
    let o = classify(root, &["--feature", "about-page", "--kind", "simple"], None);
    expect_code(&o, 0, "classify simple without marketplace");
    let v = o.json();
    assert_eq!(v["feature"], "about-page");
    assert_eq!(v["kind"], "simple");
    assert!(v["existing"].is_null(), "{v}");
    assert_eq!(
        recorded_features(root)["about-page"]["kind"].as_str(),
        Some("simple")
    );
}

fn assert_classify_dedicated_undetermined(mp: Option<&Path>, what: &str) {
    let t = TempDir::new().unwrap();
    let root = t.path();
    write_record(root, &all_irrelevant_toml());
    // Control: the same command with a valid marketplace succeeds.
    let good = fixture("marketplace.json");
    let args = [
        "--feature",
        "login",
        "--kind",
        "dedicated-harness",
        "--harness",
        "authkit",
    ];
    expect_code(
        &classify(root, &args, Some(&good)),
        0,
        &format!("{what}: control"),
    );
    let args2 = [
        "--feature",
        "upload",
        "--kind",
        "dedicated-harness",
        "--harness",
        "authkit",
    ];
    let o = classify(root, &args2, mp);
    expect_code(&o, 2, what);
    assert!(
        recorded_features(root).get("upload").is_none(),
        "{what}: an undetermined classify must not be recorded"
    );
}

#[test]
fn classify_without_marketplace_env_is_undetermined() {
    assert_classify_dedicated_undetermined(None, "SITECRAFT_MARKETPLACE unset");
}

#[test]
fn classify_with_unreadable_marketplace_is_undetermined() {
    let missing = fixture("does-not-exist.json");
    assert!(!missing.exists());
    assert_classify_dedicated_undetermined(Some(&missing), "marketplace missing");
}

#[test]
fn classify_with_invalid_marketplace_json_is_undetermined() {
    assert_classify_dedicated_undetermined(
        Some(&fixture("marketplace-invalid.json")),
        "marketplace invalid JSON",
    );
}

#[test]
fn classify_with_marketplace_lacking_plugins_is_undetermined() {
    assert_classify_dedicated_undetermined(
        Some(&fixture("marketplace-no-plugins.json")),
        "marketplace without plugins array",
    );
}

#[test]
fn classify_unknown_kind_is_undetermined() {
    let t = TempDir::new().unwrap();
    let root = t.path();
    expect_code(
        &classify(root, &["--feature", "x", "--kind", "simple"], None),
        0,
        "control",
    );
    expect_code(
        &classify(root, &["--feature", "y", "--kind", "complex"], None),
        2,
        "kind complex",
    );
}

// ---------------------------------------------------------------- marketplace lib

#[test]
fn marketplace_plugins_reads_names_and_rejects_bad_files() {
    use sitecraft::decide::marketplace_plugins;
    use sitecraft::Determination;
    match marketplace_plugins(&fixture("marketplace.json")) {
        Determination::Known(names) => {
            let want: BTreeSet<String> = ["authkit", "vrmforge"]
                .iter()
                .map(|s| s.to_string())
                .collect();
            assert_eq!(names, want);
        }
        Determination::Undetermined(why) => panic!("valid fixture undetermined: {}", why.as_str()),
    }
    for bad in [
        "marketplace-invalid.json",
        "marketplace-no-plugins.json",
        "does-not-exist.json",
    ] {
        assert!(
            matches!(
                marketplace_plugins(&fixture(bad)),
                Determination::Undetermined(_)
            ),
            "{bad} must be Undetermined"
        );
    }
}
