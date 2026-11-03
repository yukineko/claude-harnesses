//! Spec `sitecraft-capabilities` R5 — capability results and the Stop gate
//! (rulings D5).
//!
//! # Surface assumed (rulings D5)
//!
//! * Every `sitecraft --root <root> check capability <name>` run (exit 0 and
//!   exit 1 are observed here) writes
//!   `<state>/capability-results/<root-key>/<name>.json` =
//!   `{"name": <name>, "exit": <int>, "input_hash": <64 lowercase hex>,
//!   "sitecraft_version": <CARGO_PKG_VERSION>, "at": <string>}` via temp
//!   file + rename (no other file is left in that directory). Not writable
//!   (here: `<state>/capability-results` is a regular file) → the check
//!   itself exits 2.
//! * `input_hash` is a pure function of the `[capabilities.<name>]` table and
//!   the kind's input files (blender-headless: `script` + `verify.inputs`;
//!   threejs-render: `sitecraft.toml` + `web_globs` + `verify.inputs`;
//!   command: `verify.inputs`): rerunning with the same inputs gives the same
//!   value; changing one input byte, the script, or the table (e.g. `how`)
//!   changes it; changing a file outside the inputs does not. command with
//!   `inputs` matching nothing, or an unreadable input → exit 2.
//! * `sitecraft gate` (Stop payload on stdin, as in `tests/gate_paths.rs`)
//!   with a trusted `sitecraft.toml`: for each relevant capability whose
//!   record is missing / `exit != 0` / wrong `sitecraft_version` / wrong
//!   `input_hash` / unparseable / not recomputable (input unreadable) →
//!   block, reason containing the capability name and
//!   `sitecraft check capability <name>`. Fresh records → allow.
//! * The gate never spawns blender (`SITECRAFT_BLENDER` marker), Chromium
//!   (`SITECRAFT_NODE` marker) or a capability command (its marker).
//! * Bounded give-up unchanged: with default `max_attempts = 3` the 4th
//!   consecutive blocked stop of a session is allowed with a `systemMessage`
//!   containing `NOTHING WAS VERIFIED`.
//! * No relevant capability (no table, or only irrelevant ones) → the gate
//!   allows exactly as before.
//!
//! * worst_of composition: a fresh capability record does not hide another
//!   failing check (`fresh_capability_does_not_mask_another_failing_check`).
//!
//! The panic barrier has no runtime seam (it would need a panic injection
//! point inside the gate); `gate_still_runs_under_the_panic_barrier` pins it
//! structurally (the gate entries still call `run_guarded`). It holds at HEAD.
//! `no_relevant_capability_leaves_the_gate_as_before` is RED at HEAD only
//! because its complete record carries `[purpose]` (R1): HEAD rejects the
//! field, so the gate blocks undetermined.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod spec_common;

use std::ffi::OsString;

use serde_json::Value;
use spec_common::*;

/// A gate project with a relevant `command` capability `conv` whose
/// `tool.sh` leaves a marker and exits `exit`; inputs `["tool.sh", "data/*.txt"]`.
fn conv_project(exit: i32) -> GateProject {
    let g = GateProject::new("");
    g.p.write("data/in.txt", "input v1\n");
    write_tool(&g, exit);
    write_conv_decisions(&g, "");
    g
}

fn write_tool(g: &GateProject, exit: i32) {
    let marker = g.p.marker("tool-ran");
    write_exec(
        &g.p.path("tool.sh"),
        &format!(
            "#!/bin/sh\nmkdir -p '{}'\n: > '{}'\nexit {exit}\n",
            marker.parent().unwrap().display(),
            marker.display()
        ),
    );
}

fn write_conv_decisions(g: &GateProject, extra: &str) {
    let argv0 = g.p.path("tool.sh").display().to_string();
    g.p.write_decisions(&format!(
        "{}{}{extra}",
        complete_record(),
        cap_command("conv", &[&argv0], &["tool.sh", "data/*.txt"])
    ));
}

fn check_conv(g: &GateProject) -> Out {
    g.run_env(&["check", "capability", "conv"], &[])
}

fn assert_blocks_for_conv(o: &Out, what: &str) {
    let reason = assert_block(o, what);
    assert!(
        reason.contains("sitecraft check capability conv"),
        "{what}: the block reason must name `sitecraft check capability conv`: {reason}"
    );
}

#[test]
fn check_capability_writes_the_result_record() {
    let g = conv_project(0);
    g.p.trust_ok();
    expect_code(&check_conv(&g), 0, "check capability conv");
    let r = g.p.result("conv");
    assert_eq!(r["name"], "conv", "{r}");
    assert_eq!(r["exit"], 0, "{r}");
    assert_eq!(r["sitecraft_version"], env!("CARGO_PKG_VERSION"), "{r}");
    assert!(r["at"].is_string(), "`at` must be a string: {r}");
    let h = r["input_hash"].as_str().unwrap_or("");
    assert!(
        h.len() == 64
            && h.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "input_hash must be lowercase sha256 hex: {r}"
    );
    let entries: Vec<String> = std::fs::read_dir(g.p.results_dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        entries,
        vec!["conv.json".to_string()],
        "no temp file may be left behind"
    );
}

#[test]
fn a_violating_run_is_recorded_too() {
    let g = conv_project(1);
    g.p.trust_ok();
    expect_code(&check_conv(&g), 1, "check capability conv (tool exits 1)");
    assert_eq!(g.p.result("conv")["exit"], 1);
}

#[test]
fn unwritable_results_dir_makes_the_check_exit_2() {
    let g = conv_project(0);
    g.p.trust_ok();
    write(&g.p.state.join("capability-results"), "not a directory\n");
    let o = check_conv(&g);
    expect_code(
        &o,
        2,
        "check capability with an unwritable results location",
    );
    assert!(
        g.p.marker("tool-ran").exists(),
        "setup: the command itself ran"
    );
}

#[test]
fn input_hash_tracks_exactly_the_inputs_and_the_table() {
    let g = conv_project(0);
    g.p.trust_ok();
    let hash = |what: &str| {
        expect_code(&check_conv(&g), 0, what);
        g.p.result("conv")["input_hash"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let h1 = hash("first run");
    assert_eq!(hash("same inputs"), h1, "same inputs → same input_hash");
    g.p.write("README.md", "not an input\n");
    assert_eq!(
        hash("non-input file added"),
        h1,
        "a non-input file must not count"
    );
    g.p.write("data/in.txt", "input v2\n");
    let h2 = hash("one input changed");
    assert_ne!(h2, h1, "changing an input file must change input_hash");
    write_conv_decisions(&g, "");
    let table = std::fs::read_to_string(g.p.decisions_path())
        .unwrap()
        .replace("how = \"a converter script\"", "how = \"another how\"");
    g.p.write_decisions(&table);
    let h3 = hash("table changed (how)");
    assert_ne!(
        h3, h2,
        "changing the capability table must change input_hash"
    );
}

#[test]
fn blender_input_hash_covers_the_script_and_verify_inputs() {
    let g = GateProject::new("");
    g.p.write(
        "scripts/make.py",
        "import sys, shutil, os\na = sys.argv[sys.argv.index('--') + 1:]\nfor flag, src in (('--glb', 'SPEC_SRC_GLB'), ('--render', 'SPEC_SRC_PNG')):\n    dst = a[a.index(flag) + 1]\n    os.makedirs(os.path.dirname(dst), exist_ok=True)\n    shutil.copyfile(os.environ[src], dst)\n",
    );
    g.p.write("assets/tex.txt", "texture v1\n");
    g.p.write_decisions(&format!(
        "{}{}",
        complete_record(),
        cap_blender(
            "blender",
            "scripts/make.py",
            "out/m.glb",
            "out/r.png",
            &["assets/*.txt"]
        )
    ));
    let glb = g.p.base.join("src/in.glb");
    let png = g.p.base.join("src/in.png");
    write(&glb, glb_valid());
    write(&png, png_with_drawn(150, [0, 0, 0, 255]));
    let env: Vec<(&str, OsString)> = vec![
        ("SITECRAFT_BLENDER", os(g.p.fake_blender())),
        ("SPEC_SRC_GLB", os(&glb)),
        ("SPEC_SRC_PNG", os(&png)),
    ];
    let hash = |what: &str| {
        expect_code(
            &g.run_env(&["check", "capability", "blender"], &env),
            0,
            what,
        );
        g.p.result("blender")["input_hash"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let h1 = hash("first run");
    g.p.write("assets/tex.txt", "texture v2\n");
    let h2 = hash("verify.inputs file changed");
    assert_ne!(h2, h1, "a verify.inputs file must count");
    let script = std::fs::read_to_string(g.p.path("scripts/make.py")).unwrap();
    g.p.write("scripts/make.py", format!("{script}# touched\n"));
    let h3 = hash("script changed");
    assert_ne!(h3, h2, "the script must count");
}

#[test]
fn threejs_input_hash_covers_sitecraft_toml_web_globs_and_verify_inputs() {
    // GateProject: web_globs = ["src/**", "index.html"], python dev server.
    let g = GateProject::new("");
    g.p.write("assets/model.txt", "model v1\n");
    let cap = cap_threejs("threejs", "/viewer.html", None).replace(
        "page = \"/viewer.html\"\n",
        "page = \"/viewer.html\"\ninputs = [\"assets/*.txt\"]\n",
    );
    assert!(
        cap.contains("inputs = "),
        "setup: verify.inputs must be set"
    );
    g.p.write_decisions(&format!("{}{cap}", complete_record()));
    g.p.install_fake_playwright();
    g.p.trust_ok();
    let _serve = ServeGuard(&g.p);
    let shot = g.p.base.join("shot.png");
    write(&shot, png_with_drawn(150, [0, 0, 0, 255]));
    let sc = g.p.scenario(&serde_json::json!({
        "visible": true,
        "screenshot": shot.display().to_string()
    }));
    let env: Vec<(&str, OsString)> = vec![
        ("SITECRAFT_NODE", os(which("node"))),
        ("FAKE_PLAYWRIGHT_SCENARIO", os(&sc)),
    ];
    let hash = |what: &str| {
        expect_code(
            &g.run_env(&["check", "capability", "threejs"], &env),
            0,
            what,
        );
        g.p.result("threejs")["input_hash"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let h1 = hash("first run");
    g.p.write("README.md", "not an input\n");
    assert_eq!(hash("non-input file added"), h1, "README.md is no input");
    g.p.write("src/main.js", "export const answer = 43;\n");
    let h2 = hash("web_globs file changed");
    assert_ne!(h2, h1, "a web_globs file must count");
    g.p.write("assets/model.txt", "model v2\n");
    let h3 = hash("verify.inputs file changed");
    assert_ne!(h3, h2, "a verify.inputs file must count");
    let toml_text = std::fs::read_to_string(g.p.path("sitecraft.toml")).unwrap();
    g.p.write("sitecraft.toml", format!("{toml_text}# touched\n"));
    g.p.trust_ok();
    let h4 = hash("sitecraft.toml changed (re-trusted)");
    assert_ne!(h4, h3, "sitecraft.toml must count");
}

#[test]
fn command_inputs_matching_nothing_or_unreadable_are_exit_2() {
    let g = conv_project(0);
    let argv0 = g.p.path("tool.sh").display().to_string();
    g.p.write_decisions(&format!(
        "{}{}",
        complete_record(),
        cap_command("conv", &[&argv0], &["nothing/**/*.none"])
    ));
    g.p.trust_ok();
    expect_code(&check_conv(&g), 2, "command inputs matching no file");

    let g = conv_project(0);
    g.p.trust_ok();
    expect_code(&check_conv(&g), 0, "positive control: readable inputs");
    chmod(&g.p.path("data/in.txt"), 0o000);
    let o = check_conv(&g);
    chmod(&g.p.path("data/in.txt"), 0o644);
    expect_code(&o, 2, "an unreadable input file");
}

#[test]
fn gate_blocks_without_a_record_and_allows_after_the_check() {
    let g = conv_project(0);
    g.p.trust_ok();
    let o = g.gate("s-norecord", &[]);
    assert_blocks_for_conv(&o, "no capability record");
    let reason = block_reason(&o).unwrap();
    assert!(
        reason.contains("conv"),
        "reason must name the capability: {reason}"
    );
    expect_code(&check_conv(&g), 0, "check capability conv");
    assert_allow(&g.gate("s-norecord-2", &[]), "fresh record");
}

#[test]
fn gate_blocks_on_a_failed_record() {
    let g = conv_project(1);
    g.p.trust_ok();
    expect_code(&check_conv(&g), 1, "check capability conv (exit 1)");
    assert_blocks_for_conv(&g.gate("s-failed", &[]), "record with exit 1");
}

fn tamper(g: &GateProject, key: &str, value: Value) {
    let mut r = g.p.result("conv");
    r[key] = value;
    write(&g.p.results_file("conv"), r.to_string());
}

#[test]
fn gate_blocks_on_a_stale_or_broken_record() {
    let g = conv_project(0);
    g.p.trust_ok();
    expect_code(&check_conv(&g), 0, "check capability conv");
    assert_allow(&g.gate("s-fresh", &[]), "fresh record");
    let good = std::fs::read(g.p.results_file("conv")).unwrap();

    tamper(&g, "sitecraft_version", Value::from("0.0.0-other"));
    assert_blocks_for_conv(&g.gate("s-version", &[]), "sitecraft_version mismatch");
    write(&g.p.results_file("conv"), &good);

    tamper(&g, "input_hash", Value::from("0".repeat(64)));
    assert_blocks_for_conv(&g.gate("s-hash", &[]), "input_hash mismatch");
    write(&g.p.results_file("conv"), &good);

    tamper(&g, "exit", Value::from(2));
    assert_blocks_for_conv(&g.gate("s-exit2", &[]), "record exit 2");
    write(&g.p.results_file("conv"), &good);

    write(&g.p.results_file("conv"), "{ not json");
    assert_blocks_for_conv(&g.gate("s-garbage", &[]), "unparseable record");
    write(&g.p.results_file("conv"), &good);

    chmod(&g.p.path("data/in.txt"), 0o000);
    let o = g.gate("s-unreadable-input", &[]);
    chmod(&g.p.path("data/in.txt"), 0o644);
    assert_blocks_for_conv(&o, "input cannot be re-hashed");

    assert_allow(&g.gate("s-restored", &[]), "restored record");
}

#[test]
fn changing_one_input_byte_blocks_until_the_check_is_rerun() {
    let g = conv_project(0);
    g.p.trust_ok();
    expect_code(&check_conv(&g), 0, "check capability conv");
    assert_allow(&g.gate("s-byte-0", &[]), "fresh record");
    g.p.write("data/in.txt", "input v1!\n");
    assert_blocks_for_conv(&g.gate("s-byte-1", &[]), "one input byte changed");
    assert_blocks_for_conv(&g.gate("s-byte-2", &[]), "still stale");
    expect_code(&check_conv(&g), 0, "rerun check capability conv");
    assert_allow(&g.gate("s-byte-3", &[]), "after the rerun");
}

#[test]
fn gate_spawns_no_capability() {
    let g = conv_project(0);
    g.p.write("scripts/make.py", "print('never run by the gate')\n");
    let argv0 = g.p.path("tool.sh").display().to_string();
    g.p.write_decisions(&format!(
        "{}{}{}{}",
        complete_record(),
        cap_command("conv", &[&argv0], &["tool.sh", "data/*.txt"]),
        cap_blender("blender", "scripts/make.py", "out/m.glb", "out/r.png", &[]),
        cap_threejs("threejs", "/viewer.html", None)
    ));
    g.p.trust_ok();
    g.p.install_fake_playwright();
    std::fs::create_dir_all(g.p.base.join("markers")).unwrap();
    let env: Vec<(&str, OsString)> = vec![
        ("SITECRAFT_BLENDER", os(g.p.fake_blender())),
        ("FAKE_BLENDER_MARKER", os(g.p.marker("blender-ran"))),
    ];
    let o = g.gate("s-nospawn", &env);
    let reason = assert_block(&o, "three capabilities without records");
    for name in ["conv", "blender", "threejs"] {
        assert!(
            reason.contains(&format!("sitecraft check capability {name}")),
            "reason must name {name}: {reason}"
        );
    }
    assert!(
        !g.p.marker("tool-ran").exists(),
        "the gate ran the capability command"
    );
    assert!(!g.p.marker("blender-ran").exists(), "the gate ran blender");
    assert!(
        !g.p.marker("node-ran").exists(),
        "the gate ran node (Chromium)"
    );
    assert!(
        !g.p.marker("dev-server-ran").exists(),
        "the gate started the dev server for a capability"
    );
}

#[test]
fn gate_does_not_rerun_a_freshly_checked_command() {
    let g = conv_project(0);
    g.p.trust_ok();
    expect_code(&check_conv(&g), 0, "check capability conv");
    std::fs::remove_file(g.p.marker("tool-ran")).unwrap();
    assert_allow(&g.gate("s-norerun", &[]), "fresh record");
    assert!(!g.p.marker("tool-ran").exists(), "the gate ran the command");
}

#[test]
fn stale_capability_keeps_the_bounded_give_up() {
    let g = conv_project(0);
    g.p.trust_ok();
    for attempt in 1..=3 {
        assert_blocks_for_conv(
            &g.gate("s-giveup", &[]),
            &format!("attempt {attempt} with no record"),
        );
    }
    let o = g.gate("s-giveup", &[]);
    assert_eq!(o.code, Some(0), "{}", o.describe());
    assert!(
        block_reason(&o).is_none(),
        "4th stop must be allowed\n{}",
        o.describe()
    );
    assert!(
        o.stdout.contains("NOTHING WAS VERIFIED"),
        "the give-up must say NOTHING WAS VERIFIED\n{}",
        o.describe()
    );
}

#[test]
fn fresh_capability_does_not_mask_another_failing_check() {
    // worst_of composition: a clean capability verdict must not hide a
    // failing existing check (here: decide check with a pending area).
    let g = conv_project(0);
    g.p.trust_ok();
    expect_code(&check_conv(&g), 0, "check capability conv");
    assert_allow(
        &g.gate("s-compose-0", &[]),
        "fresh record, complete decisions",
    );
    let argv0 = g.p.path("tool.sh").display().to_string();
    let pending: String = AREAS
        .iter()
        .filter(|a| **a != "auth")
        .map(|a| format!("[areas.{a}]\nrelevance = \"irrelevant\"\n\n"))
        .collect();
    g.p.write_decisions(&format!(
        "{}{pending}{}",
        purpose("Spec test purpose."),
        cap_command("conv", &[&argv0], &["tool.sh", "data/*.txt"])
    ));
    let reason = assert_block(
        &g.gate("s-compose-1", &[]),
        "fresh capability record but the auth area is undecided",
    );
    assert!(
        reason.contains("decide"),
        "the block must come from the decide check: {reason}"
    );
    assert!(
        !reason.contains("sitecraft check capability conv"),
        "the capability record is fresh (its table did not change), so it must not be blamed: {reason}"
    );
}

#[test]
fn gate_still_runs_under_the_panic_barrier() {
    // The panic barrier has no runtime seam (no panic injection point), so
    // its presence is pinned structurally: every `sitecraft gate` entry goes
    // through `harness_core::gate::run::run_guarded`, and the
    // turn-never-broken `run_hook` entry is not used. Holds at HEAD; guards
    // R5 "panic barrier は変わらない".
    let src = std::fs::read_to_string(crate_dir().join("src/gate.rs")).expect("read src/gate.rs");
    let guarded = src
        .matches("harness_core::gate::run::run_guarded(GATE_NAME")
        .count();
    assert!(
        guarded >= 3,
        "the manual, hook and unparseable-payload entries must each run under run_guarded (found {guarded})"
    );
    assert!(
        !src.contains("harness_core::hook::run_hook"),
        "the gate must not use the fail-open run_hook entry"
    );
}

#[test]
fn no_relevant_capability_leaves_the_gate_as_before() {
    let g = GateProject::new("");
    g.p.trust_ok();
    assert_allow(&g.gate("s-nocaps", &[]), "no [capabilities]");

    let irrelevant = cap_command("conv", &["./tool.sh"], &["tool.sh"])
        .replace("relevance = \"relevant\"", "relevance = \"irrelevant\"");
    let g = GateProject::new(&irrelevant);
    g.p.trust_ok();
    assert_allow(
        &g.gate("s-irrelevant", &[]),
        "only an irrelevant capability",
    );
}
