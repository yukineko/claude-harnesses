//! Spec `sitecraft-capabilities` R11 — time limits, check history,
//! fingerprints and `sitecraft status` (rulings D11, D12, C2, U5, U6).
//!
//! # Surface assumed (rulings D11)
//!
//! * `decide check`, `check pages`, `check security`, `check capability <n>`
//!   (CLI, and the same checks run by `sitecraft gate`) append one line per
//!   run to `<state>/check-history/<root-key>.jsonl`:
//!   `{"name": "decide"|"pages"|"security"|"capability:<n>", "exit": <int>,
//!   "fingerprint": <64 hex>|null, "elapsed_ms": <int>, "timed_out": <bool>,
//!   "at": <string>}`. Not appendable (here: `<state>/check-history` is a
//!   regular file) → that check exits 2.
//! * `fingerprint`: exit 0 → null; time-out → sha256("timeout"); otherwise
//!   sha256 hex of stdout+stderr with RFC 3339 times → `<time>`, absolute
//!   paths → `<path>`, digit runs → `0`. Observed: two failures differing only
//!   in the root path (absent-record message) or only in a time and digits
//!   (a pending domain question id) share a fingerprint; ids `alpha` vs
//!   `beta` do not.
//! * `sitecraft --root <root> status` prints
//!   `{"checks":[{"name","last_exit","consecutive_same_failure","elapsed_ms","state"}]}`
//!   (one entry per name in the history). `consecutive_same_failure` counts
//!   from the newest line back while `exit != 0` and the fingerprint equals
//!   the newest one. State priority: `waiting-human` (pages, or a
//!   threejs-render / command capability, while `sitecraft.toml` or the
//!   D2-trust digest is untrusted) > `stuck` (>= 3) > `failing` > `slow`
//!   (exit 0 and elapsed_ms >= 50% of the check's limit; capability limit
//!   300 s; decide is never slow) > `ok`. Exit 0 when every state is ok/slow,
//!   1 when any is failing/stuck/waiting-human, 2 when the history is absent,
//!   unreadable, has an unparseable line, or the state dir cannot be found.
//! * Time limits: the api_paths 10 s limit is observed here (exit 2 and
//!   `timeout` in the output); probe 120 s / check capability 300 s / init
//!   git 30 s are in spec_r3 / spec_r4 / spec_r6.
//! * SKILL.md: stuck → stop the re-run loop, classify with D12 (a)–(d),
//!   harness side → `backlog add` with a RED-observed reproduction test in a
//!   harness-repo worktree, fix + rollout, then resume; app side → Ask after
//!   3 more stuck with the same fingerprint; waiting-human is not a stall →
//!   Ask about the file needing `sitecraft trust`, do not keep waiting; scope
//!   limited to the `/sitecraft` build loop (U6).
//!
//! `#[ignore]`d: `timed_out_check_has_the_timeout_fingerprint` (check pages
//! needs its 105 s deadline to pass).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod spec_common;

use std::time::Duration;

use serde_json::{json, Value};
use spec_common::*;

fn last_named(p: &Project, name: &str) -> Value {
    p.check_history()
        .into_iter()
        .rev()
        .find(|l| l["name"] == name)
        .unwrap_or_else(|| panic!("no `{name}` line in the history: {:?}", p.check_history()))
}

#[track_caller]
fn assert_line_shape(l: &Value, exit: i64) {
    assert_eq!(l["exit"], exit, "{l}");
    assert!(
        l["elapsed_ms"].is_u64(),
        "elapsed_ms must be an integer: {l}"
    );
    assert!(l["timed_out"].is_boolean(), "timed_out must be a bool: {l}");
    assert!(l["at"].is_string(), "at must be a string: {l}");
    if exit == 0 {
        assert!(l["fingerprint"].is_null(), "exit 0 → fingerprint null: {l}");
    } else {
        let f = l["fingerprint"].as_str().unwrap_or("");
        assert!(
            f.len() == 64 && f.chars().all(|c| c.is_ascii_hexdigit()),
            "exit != 0 → sha256 hex fingerprint: {l}"
        );
    }
}

#[test]
fn every_check_appends_a_history_line() {
    let g = GateProject::new("");
    g.p.write("data/in.txt", "x\n");
    write_exec(&g.p.path("tool.sh"), "#!/bin/sh\nexit 0\n");
    let argv0 = g.p.path("tool.sh").display().to_string();
    g.p.write_decisions(&format!(
        "{}{}",
        complete_record(),
        cap_command("conv", &[&argv0], &["tool.sh"])
    ));
    g.p.trust_ok();
    let pages_node = g.p.base.join("bin/pages-node");
    write_exec(
        &pages_node,
        "#!/bin/sh\necho '{\"schema\":\"sitecraft-check-pages/1\",\"status\":\"ok\",\"pages_checked\":1,\"findings\":[]}'\nexit 0\n",
    );
    expect_code(&g.run_env(&["decide", "check"], &[]), 0, "decide check");
    assert_line_shape(&last_named(&g.p, "decide"), 0);
    expect_code(
        &g.run_env(&["check", "pages"], &[("SITECRAFT_NODE", os(&pages_node))]),
        0,
        "check pages",
    );
    assert_line_shape(&last_named(&g.p, "pages"), 0);
    expect_code(&g.run_env(&["check", "security"], &[]), 0, "check security");
    assert_line_shape(&last_named(&g.p, "security"), 0);
    expect_code(
        &g.run_env(&["check", "capability", "conv"], &[]),
        0,
        "check capability",
    );
    assert_line_shape(&last_named(&g.p, "capability:conv"), 0);

    // A failing run is recorded with a fingerprint.
    g.p.write_decisions(&areas_all_irrelevant());
    let o = g.run_env(&["decide", "check"], &[]);
    expect_code(&o, 1, "decide check without purpose");
    assert_line_shape(&last_named(&g.p, "decide"), 1);
}

#[test]
fn the_gate_appends_history_lines_too() {
    let g = GateProject::new("");
    g.p.trust_ok();
    let before = g.p.check_history_len();
    assert_allow(&g.gate("s-hist", &[]), "clean baseline");
    let lines = g.p.check_history();
    let new: Vec<&Value> = lines[before..].iter().collect();
    for name in ["decide", "security"] {
        assert!(
            new.iter().any(|l| l["name"] == name),
            "the gate's `{name}` check must append a line: {new:?}"
        );
    }
}

#[test]
fn unappendable_history_makes_the_check_exit_2() {
    let p = Project::new();
    p.write_decisions(&complete_record());
    expect_code(&p.run(&["decide", "check"]), 0, "positive control");
    assert!(
        p.history_file().exists(),
        "positive control: the history line was written"
    );
    std::fs::remove_dir_all(p.state.join("check-history")).unwrap();
    write(&p.state.join("check-history"), "not a directory\n");
    expect_code(
        &p.run(&["decide", "check"]),
        2,
        "history cannot be appended",
    );
}

#[test]
fn failures_differing_only_in_paths_share_a_fingerprint() {
    // Absent records: the message names each root's absolute path.
    let a = Project::new();
    let b = Project::new();
    let b_root_out = b.run(&["decide", "check"]);
    let a_out = a.run(&["decide", "check"]);
    expect_code(&a_out, 2, "absent record (a)");
    expect_code(&b_root_out, 2, "absent record (b)");
    let fa = last_named(&a, "decide")["fingerprint"].clone();
    let fb = last_named(&b, "decide")["fingerprint"].clone();
    assert!(fa.is_string(), "{fa}");
    assert_eq!(
        fa, fb,
        "only the root path differs, so the fingerprints must match"
    );
}

fn domain_pending(id: &str) -> Project {
    let p = Project::new();
    p.write_decisions(&format!(
        "{}[[questions]]\nid = {}\ntext = \"Pending question\"\n\n",
        complete_record(),
        toml_str(id)
    ));
    p
}

#[test]
fn failures_differing_only_in_times_and_digits_share_a_fingerprint() {
    let a = domain_pending("run-2026-11-03T10:00:00Z-n1");
    let b = domain_pending("run-2025-01-02T03:04:05Z-n22");
    let oa = a.run(&["decide", "check"]);
    let ob = b.run(&["decide", "check"]);
    expect_code(&oa, 1, "pending domain question (a)");
    expect_code(&ob, 1, "pending domain question (b)");
    assert!(
        oa.all().contains("run-2026-11-03T10:00:00Z-n1"),
        "setup: the id is printed"
    );
    let fa = last_named(&a, "decide")["fingerprint"].clone();
    let fb = last_named(&b, "decide")["fingerprint"].clone();
    assert!(fa.is_string(), "{fa}");
    assert_eq!(
        fa, fb,
        "only a time and digits differ, so the fingerprints must match"
    );

    // Negative control: a real textual difference changes the fingerprint.
    let c = domain_pending("alpha");
    let d = domain_pending("beta");
    expect_code(&c.run(&["decide", "check"]), 1, "pending alpha");
    expect_code(&d.run(&["decide", "check"]), 1, "pending beta");
    assert_ne!(
        last_named(&c, "decide")["fingerprint"],
        last_named(&d, "decide")["fingerprint"],
        "different failures must not share a fingerprint"
    );
}

#[test]
#[ignore = "takes ~105 s: check pages' own deadline (30 + 45 x 1 page + 30 s) must expire"]
fn timed_out_check_has_the_timeout_fingerprint() {
    let p = Project::new();
    p.write_sitecraft_toml("");
    let node = p.base.join("bin/slow-node");
    write_exec(&node, "#!/bin/sh\nsleep 200\n");
    let o = p.run_env(&["check", "pages"], &[("SITECRAFT_NODE", os(&node))]);
    expect_code(&o, 2, "check pages past its deadline");
    let l = last_named(&p, "pages");
    assert_eq!(l["timed_out"], true, "{l}");
    assert_eq!(l["fingerprint"], sha256_hex(b"timeout"), "{l}");
}

#[test]
fn api_path_time_limit_reports_timeout() {
    let p = Project::new();
    let port = p.write_sitecraft_toml("api_paths = [\"/api/hang\"]\n");
    let l = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
    let accepted = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = std::sync::Arc::clone(&accepted);
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for s in l.incoming().flatten() {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            held.push(s); // accept, never answer
        }
    });
    let node = p.base.join("bin/node-ok");
    write_exec(
        &node,
        "#!/bin/sh\necho '{\"schema\":\"sitecraft-check-pages/1\",\"status\":\"ok\",\"pages_checked\":1,\"findings\":[]}'\nexit 0\n",
    );
    let o = p.run_env(&["check", "pages"], &[("SITECRAFT_NODE", os(&node))]);
    expect_code(&o, 2, "api path past 10 s");
    assert!(
        accepted.load(std::sync::atomic::Ordering::SeqCst) >= 1,
        "the exit 2 must come from the unanswered API request (none arrived)\n{}",
        o.describe()
    );
    // `ready_timeout_secs` (a config key name, e.g. in a parse error) is
    // not a timeout report.
    assert!(
        o.all()
            .replace("ready_timeout_secs", "")
            .contains("timeout"),
        "the output must report `timeout`\n{}",
        o.describe()
    );
    assert!(
        o.elapsed >= Duration::from_secs(9) && o.elapsed < Duration::from_secs(40),
        "the per-path limit is 10 s: {:?}",
        o.elapsed
    );
}

#[test]
fn existing_time_limits_are_unchanged() {
    // R11: "既存の deadline_for・npm audit 300 秒・ready_timeout_secs は変わらない".
    // Pinned structurally (the runtime check of the pages deadline is the
    // ~105 s ignored test above). Holds at HEAD; guards the new limits work.
    let src = |rel: &str| {
        std::fs::read_to_string(crate_dir().join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
    };
    let pages = src("src/check/pages.rs");
    assert!(
        pages.contains("30u64.saturating_add(45u64.saturating_mul(pages as u64))")
            && pages.contains("Duration::from_secs(secs.min(480) + 30)"),
        "deadline_for must stay 30 + 45 x pages (cap 480) + 30 s"
    );
    let security = src("src/check/security.rs");
    assert!(
        security.contains("const AUDIT_TIMEOUT: Duration = Duration::from_secs(300);"),
        "npm audit must keep its 300 s limit"
    );
    assert!(
        src("src/config.rs").contains("ready_timeout_secs"),
        "serve start must keep the ready_timeout_secs wait"
    );
}

// ---------------------------------------------------------------- status

fn hist(name: &str, exit: i64, fp: Option<&str>, elapsed_ms: u64) -> String {
    json!({
        "name": name,
        "exit": exit,
        "fingerprint": fp,
        "elapsed_ms": elapsed_ms,
        "timed_out": false,
        "at": "2026-11-03T00:00:00Z",
    })
    .to_string()
}

fn write_history(p: &Project, lines: &[String]) {
    write(&p.history_file(), lines.join("\n") + "\n");
}

fn status(p: &Project) -> (Out, Value) {
    let o = p.run(&["status"]);
    let v = o
        .json_objects()
        .pop()
        .unwrap_or_else(|| panic!("status printed no JSON\n{}", o.describe()));
    (o, v)
}

fn entry(v: &Value, name: &str) -> Value {
    v["checks"]
        .as_array()
        .unwrap_or_else(|| panic!("no `checks` array: {v}"))
        .iter()
        .find(|c| c["name"] == name)
        .cloned()
        .unwrap_or_else(|| panic!("no `{name}` entry: {v}"))
}

const FA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const FB: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn decide_project() -> Project {
    let p = Project::new();
    p.write_decisions(&complete_record());
    p
}

#[test]
fn three_same_failures_are_stuck() {
    let p = decide_project();
    write_history(
        &p,
        &[
            hist("decide", 1, Some(FA), 5),
            hist("decide", 1, Some(FB), 5),
            hist("decide", 1, Some(FB), 5),
            hist("decide", 1, Some(FB), 5),
        ],
    );
    let (o, v) = status(&p);
    let e = entry(&v, "decide");
    assert_eq!(e["state"], "stuck", "{e}");
    assert_eq!(e["consecutive_same_failure"], 3, "{e}");
    assert_eq!(e["last_exit"], 1, "{e}");
    assert!(e["elapsed_ms"].is_u64(), "{e}");
    expect_code(&o, 1, "status with a stuck check");
}

#[test]
fn fewer_or_interrupted_failures_are_failing() {
    for (what, lines, n) in [
        (
            "two same",
            vec![
                hist("decide", 1, Some(FA), 5),
                hist("decide", 1, Some(FA), 5),
            ],
            2,
        ),
        (
            "fingerprint changed",
            vec![
                hist("decide", 1, Some(FB), 5),
                hist("decide", 1, Some(FB), 5),
                hist("decide", 1, Some(FA), 5),
            ],
            1,
        ),
        (
            "interrupted by a success",
            vec![
                hist("decide", 1, Some(FB), 5),
                hist("decide", 1, Some(FB), 5),
                hist("decide", 0, None, 5),
                hist("decide", 1, Some(FB), 5),
            ],
            1,
        ),
    ] {
        let p = decide_project();
        write_history(&p, &lines);
        let (o, v) = status(&p);
        let e = entry(&v, "decide");
        assert_eq!(e["state"], "failing", "{what}: {e}");
        assert_eq!(e["consecutive_same_failure"], n, "{what}: {e}");
        expect_code(&o, 1, what);
    }
}

#[test]
fn passing_checks_are_ok_and_decide_is_never_slow() {
    let p = decide_project();
    write_history(
        &p,
        &[
            hist("decide", 1, Some(FA), 5),
            hist("decide", 0, None, 10_000_000),
        ],
    );
    let (o, v) = status(&p);
    let e = entry(&v, "decide");
    assert_eq!(e["state"], "ok", "{e}");
    assert_eq!(e["consecutive_same_failure"], 0, "{e}");
    expect_code(&o, 0, "status with every check ok");
}

#[test]
fn capability_at_half_its_limit_is_slow() {
    let p = Project::new();
    p.write_decisions(&format!(
        "{}{}",
        complete_record(),
        cap_blender("blender", "make.py", "o.glb", "o.png", &[])
    ));
    write_history(&p, &[hist("capability:blender", 0, None, 160_000)]);
    let (o, v) = status(&p);
    assert_eq!(entry(&v, "capability:blender")["state"], "slow", "{v}");
    expect_code(&o, 0, "slow is not a failure");

    write_history(&p, &[hist("capability:blender", 0, None, 140_000)]);
    let (o, v) = status(&p);
    assert_eq!(entry(&v, "capability:blender")["state"], "ok", "{v}");
    expect_code(&o, 0, "ok");
}

#[test]
fn untrusted_pages_are_waiting_human_even_when_stuck() {
    let p = Project::new();
    p.write_sitecraft_toml("");
    p.write_decisions(&complete_record());
    write_history(&p, &[hist("pages", 0, None, 5)]);
    let (o, v) = status(&p);
    assert_eq!(entry(&v, "pages")["state"], "waiting-human", "{v}");
    expect_code(&o, 1, "waiting-human");

    write_history(
        &p,
        &[
            hist("pages", 2, Some(FA), 5),
            hist("pages", 2, Some(FA), 5),
            hist("pages", 2, Some(FA), 5),
        ],
    );
    let (_o, v) = status(&p);
    assert_eq!(
        entry(&v, "pages")["state"],
        "waiting-human",
        "waiting-human outranks stuck: {v}"
    );

    // Positive control: trusted → ordinary states.
    p.trust_ok();
    write_history(&p, &[hist("pages", 0, None, 5)]);
    let (o, v) = status(&p);
    assert_eq!(entry(&v, "pages")["state"], "ok", "{v}");
    expect_code(&o, 0, "trusted pages ok");
}

#[test]
fn command_capability_without_digest_trust_is_waiting_human_but_blender_is_not() {
    let p = Project::new();
    p.write_sitecraft_toml("");
    p.write_decisions(&format!(
        "{}{}{}",
        complete_record(),
        cap_command("conv", &["./tool.sh"], &["tool.sh"]),
        cap_blender("blender", "make.py", "o.glb", "o.png", &[])
    ));
    let three =
        |name: &str| -> Vec<String> { (0..3).map(|_| hist(name, 1, Some(FA), 5)).collect() };
    let mut lines = three("capability:conv");
    lines.extend(three("capability:blender"));
    write_history(&p, &lines);
    let (o, v) = status(&p);
    assert_eq!(
        entry(&v, "capability:conv")["state"],
        "waiting-human",
        "{v}"
    );
    assert_eq!(
        entry(&v, "capability:blender")["state"],
        "stuck",
        "blender-headless needs no trust, so it is stuck, not waiting: {v}"
    );
    expect_code(&o, 1, "status");
    p.trust_ok();
    let (_o, v) = status(&p);
    assert_eq!(
        entry(&v, "capability:conv")["state"],
        "stuck",
        "after trust: {v}"
    );
}

#[test]
fn status_without_a_usable_history_is_exit_2() {
    let p = decide_project();
    expect_code(&p.run(&["status"]), 2, "no history at all");

    write_history(&p, &[hist("decide", 0, None, 5), "{ not json".to_string()]);
    expect_code(&p.run(&["status"]), 2, "history with an unparseable line");

    write_history(&p, &[hist("decide", 0, None, 5)]);
    let (o, _v) = status(&p);
    expect_code(&o, 0, "positive control: a valid history");

    chmod(&p.history_file(), 0o000);
    let o = p.run(&["status"]);
    chmod(&p.history_file(), 0o644);
    expect_code(&o, 2, "unreadable history");

    let mut c = p.cmd(&["status"]);
    c.env_remove("SITECRAFT_STATE_DIR").env_remove("HOME");
    let o = run_deadline(c, None, RUN_DEADLINE);
    expect_code(&o, 2, "no state dir");
}

// ---------------------------------------------------------------- SKILL.md

#[test]
fn skill_handles_stuck_with_the_d12_procedure() {
    let s = skill_text();
    assert!(s.contains("stuck"), "SKILL.md must handle `stuck`");
    for mark in ["(a)", "(b)", "(c)", "(d)"] {
        assert!(
            s.contains(mark),
            "SKILL.md must list the D12 criterion {mark}"
        );
    }
    assert_paragraph_with(
        &s,
        &["backlog add", "RED", "worktree"],
        "R11: harness-side stalls are filed with a RED-observed reproduction test from a worktree",
    );
    assert!(
        s.contains("rollout"),
        "SKILL.md must resume only after fix + rollout"
    );
    assert!(
        s.contains("sitecraft status"),
        "SKILL.md must use `sitecraft status`"
    );
    assert_paragraph_with(
        &s,
        &["stuck", "3", "Ask"],
        "R11: app-side stuck with the same fingerprint 3 more times → Ask",
    );
}

#[test]
fn skill_treats_waiting_human_as_an_ask_and_scopes_to_the_build_loop() {
    let s = skill_text();
    assert_paragraph_with(
        &s,
        &["waiting-human", "sitecraft trust", "Ask"],
        "R11: waiting-human → Ask about the file needing trust",
    );
    assert!(
        s.contains("ビルドループ"),
        "SKILL.md must scope this to the /sitecraft build loop"
    );
    let scope = s
        .lines()
        .any(|l| l.contains("ビルドループ") && l.contains("/sitecraft"));
    assert!(
        scope,
        "the scope line must name the `/sitecraft` build loop (U6)"
    );
}
