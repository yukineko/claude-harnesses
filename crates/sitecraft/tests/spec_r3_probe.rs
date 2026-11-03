//! Spec `sitecraft-capabilities` R3 — `sitecraft capability probe <name>`
//! (rulings D3, U2).
//!
//! # CLI surface assumed (rulings D3)
//!
//! * `sitecraft --root <root> capability probe <name>` prints exactly one
//!   non-empty stdout line, a JSON object
//!   `{"capability": <name>, "result": "feasible"|"infeasible"|"undetermined", "reason": …}`
//!   (`reason` key always present), and exits feasible 0 / infeasible 1 /
//!   undetermined 2. An unregistered name → exit 2.
//! * Time limit 120 s → `result = "undetermined"`, `reason = "timeout"`, exit 2.
//! * Blender lookup: `SITECRAFT_BLENDER` non-empty → only that (missing or
//!   not executable → undetermined); else `blender` on `PATH`; else
//!   `/Applications/Blender.app/Contents/MacOS/Blender`; none → infeasible.
//!   Found → `<blender> --background --factory-startup --python <minimal script>`
//!   (these are the first four argv elements). The minimal script exports the
//!   default scene with `bpy.ops.export_scene.gltf(filepath=<path under
//!   <state>/probe-tmp/>)`; exit 0 and a D4-valid GLB → feasible; non-zero
//!   exit or an invalid / missing GLB → infeasible; spawn failure → undetermined.
//!   (The fake Blender in `tests/fixtures/spec/fake_blender.py` runs the
//!   script with python3 against the fake `bpy` documented in
//!   `tests/fixtures/spec/fake_bpy/bpy/__init__.py`.)
//! * three.js probe: `$SITECRAFT_NODE` (else `node`) runs a bundled script
//!   with cwd = root that resolves `playwright` like `check pages`
//!   (`createRequire(<root>/…)`), launches Chromium headless and asks a
//!   canvas for `webgl2`, then `webgl`, via `page.evaluate`. Either → feasible;
//!   neither → infeasible; Playwright or Chromium cannot start → undetermined.
//!   The fake Playwright surface is documented in
//!   `tests/fixtures/spec/fake_playwright/index.js`.
//! * `command` probe: `argv[0]` containing `/` is a root-relative or absolute
//!   path that must be an executable file; otherwise it is looked up on
//!   `PATH`. Never executed, no trust required.
//! * Probing writes neither `<state>/capability-results/` nor decisions.toml.
//! * SKILL.md: a paragraph with `capability probe` and `AskUserQuestion`; a
//!   paragraph with `feasible` and `完成` (feasible is not completion, U2).
//!
//! `#[ignore]`d: `probe_times_out_after_120_seconds` (needs > 120 s) and
//! `no_blender_anywhere_is_infeasible` (only meaningful on a machine without
//! `/Applications/Blender.app`; this Mac has one, so the result would come from
//! the real Blender).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod spec_common;

use std::ffi::OsString;
use std::path::Path;

use serde_json::Value;
use spec_common::*;

/// Run the probe, assert the one-JSON-object stdout and the result/exit
/// agreement, return (output, json).
fn probe(p: &Project, name: &str, envs: &[(&str, OsString)]) -> (Out, Value) {
    probe_with_deadline(p, name, envs, RUN_DEADLINE)
}

fn probe_with_deadline(
    p: &Project,
    name: &str,
    envs: &[(&str, OsString)],
    deadline: std::time::Duration,
) -> (Out, Value) {
    let o = p.run_env_with_deadline(&["capability", "probe", name], envs, deadline);
    let lines: Vec<&str> = o.stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(
        lines.len(),
        1,
        "probe stdout must be exactly one JSON object\n{}",
        o.describe()
    );
    let v: Value = serde_json::from_str(lines[0])
        .unwrap_or_else(|e| panic!("probe stdout is not JSON ({e})\n{}", o.describe()));
    assert!(v.is_object(), "probe output must be an object: {v}");
    assert_eq!(v["capability"], name, "{v}");
    assert!(
        v.get("reason").is_some(),
        "probe JSON needs a `reason` key: {v}"
    );
    let want = match v["result"].as_str() {
        Some("feasible") => 0,
        Some("infeasible") => 1,
        Some("undetermined") => 2,
        other => panic!("result must be feasible|infeasible|undetermined, got {other:?}: {v}"),
    };
    expect_code(
        &o,
        want,
        &format!("probe exit must match result {}", v["result"]),
    );
    (o, v)
}

fn blender_project() -> Project {
    let p = Project::new();
    p.write_decisions(&format!(
        "{}{}",
        complete_record(),
        cap_blender("blender", "scripts/make.py", "out/m.glb", "out/r.png", &[])
    ));
    p
}

fn blender_env(p: &Project, extra: &[(&'static str, &str)]) -> Vec<(&'static str, OsString)> {
    let mut v: Vec<(&'static str, OsString)> = vec![
        ("SITECRAFT_BLENDER", os(p.fake_blender())),
        ("FAKE_BLENDER_LOG", os(p.base.join("blender-log.jsonl"))),
        ("FAKE_BLENDER_MARKER", os(p.marker("blender-ran"))),
        ("FAKE_BPY_LOG", os(p.base.join("bpy-log.jsonl"))),
    ];
    std::fs::create_dir_all(p.base.join("markers")).unwrap();
    for (k, val) in extra {
        v.push((*k, os(val)));
    }
    v
}

fn jsonl(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("jsonl line"))
        .collect()
}

#[test]
fn blender_probe_with_a_valid_glb_is_feasible() {
    let p = blender_project();
    let (_o, v) = probe(&p, "blender", &blender_env(&p, &[]));
    assert_eq!(v["result"], "feasible", "{v}");
    let calls = jsonl(&p.base.join("blender-log.jsonl"));
    assert_eq!(calls.len(), 1, "blender must be spawned once: {calls:?}");
    let argv: Vec<&str> = calls[0]["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    assert!(
        argv.len() >= 4 && argv[..3] == ["--background", "--factory-startup", "--python"],
        "blender argv must start with --background --factory-startup --python <script>: {argv:?}"
    );
    let exports: Vec<Value> = jsonl(&p.base.join("bpy-log.jsonl"))
        .into_iter()
        .filter(|c| c["op"] == "bpy.ops.export_scene.gltf")
        .collect();
    assert!(
        !exports.is_empty(),
        "the minimal script must export with bpy.ops.export_scene.gltf"
    );
    let probe_tmp = p.state.join("probe-tmp");
    for e in &exports {
        let fp = e["kwargs"]["filepath"].as_str().unwrap_or("");
        assert!(
            Path::new(fp).starts_with(&probe_tmp),
            "the probe GLB must be written under {}: {fp}",
            probe_tmp.display()
        );
    }
}

#[test]
fn unregistered_name_is_exit_2() {
    let p = blender_project();
    let o = p.run_env(&["capability", "probe", "nope"], &blender_env(&p, &[]));
    expect_code(&o, 2, "probe of an unregistered capability");
    // Positive control: the registered one probes feasible in the same root.
    let (_o, v) = probe(&p, "blender", &blender_env(&p, &[]));
    assert_eq!(v["result"], "feasible", "{v}");
}

#[test]
#[ignore = "takes > 120 s: the probe time limit is 120 s (rulings D3)"]
fn probe_times_out_after_120_seconds() {
    let p = blender_project();
    // Harness deadline 200 s: above the 120 s limit under test and above the
    // `< 190` bound asserted below (the default 120 s would kill sitecraft first).
    let (o, v) = probe_with_deadline(
        &p,
        "blender",
        &blender_env(&p, &[("FAKE_BLENDER_SLEEP", "200")]),
        std::time::Duration::from_secs(200),
    );
    assert_eq!(v["result"], "undetermined", "{v}");
    assert_eq!(v["reason"], "timeout", "{v}");
    assert!(
        o.elapsed.as_secs() < 190,
        "the probe must stop at its 120 s limit: {:?}",
        o.elapsed
    );
}

#[test]
fn sitecraft_blender_pointing_at_nothing_is_undetermined_and_path_is_not_used() {
    let p = blender_project();
    // A `blender` on PATH that must NOT be consulted.
    let bin = p.base.join("pathbin");
    write_exec(&bin.join("blender"), &fake_blender_wrapper());
    let path = format!("{}:/usr/bin:/bin", bin.display());
    let mut env = blender_env(&p, &[]);
    env.retain(|(k, _)| *k != "SITECRAFT_BLENDER");
    env.push(("SITECRAFT_BLENDER", os(p.base.join("no-such-blender"))));
    env.push(("PATH", os(&path)));
    let (_o, v) = probe(&p, "blender", &env);
    assert_eq!(v["result"], "undetermined", "{v}");
    assert!(
        !p.marker("blender-ran").exists(),
        "with SITECRAFT_BLENDER set, PATH's blender must not run"
    );
}

#[test]
fn sitecraft_blender_not_executable_is_undetermined() {
    let p = blender_project();
    let plain = p.base.join("bin/plain-blender");
    write(&plain, fake_blender_wrapper());
    chmod(&plain, 0o644);
    let mut env = blender_env(&p, &[]);
    env.retain(|(k, _)| *k != "SITECRAFT_BLENDER");
    env.push(("SITECRAFT_BLENDER", os(&plain)));
    let (_o, v) = probe(&p, "blender", &env);
    assert_eq!(v["result"], "undetermined", "{v}");
}

#[test]
fn blender_spawn_failure_is_undetermined() {
    let p = blender_project();
    let broken = p.base.join("bin/broken-blender");
    write_exec(&broken, "#!/nonexistent/interpreter/for/spec\nexit 0\n");
    let mut env = blender_env(&p, &[]);
    env.retain(|(k, _)| *k != "SITECRAFT_BLENDER");
    env.push(("SITECRAFT_BLENDER", os(&broken)));
    let (_o, v) = probe(&p, "blender", &env);
    assert_eq!(v["result"], "undetermined", "{v}");
}

#[test]
fn blender_on_path_is_used_when_sitecraft_blender_is_unset() {
    let p = blender_project();
    let bin = p.base.join("pathbin");
    write_exec(&bin.join("blender"), &fake_blender_wrapper());
    let path = format!("{}:/usr/bin:/bin", bin.display());
    let mut env = blender_env(&p, &[]);
    env.retain(|(k, _)| *k != "SITECRAFT_BLENDER");
    env.push(("PATH", os(&path)));
    let (_o, v) = probe(&p, "blender", &env);
    assert_eq!(v["result"], "feasible", "{v}");
    assert!(
        p.marker("blender-ran").exists(),
        "the PATH blender must have been run"
    );
}

#[test]
fn empty_sitecraft_blender_falls_back_to_path() {
    // "SITECRAFT_BLENDER が空でなければそれだけを使い": an empty value is
    // not a choice, so the PATH lookup applies.
    let p = blender_project();
    let bin = p.base.join("pathbin");
    write_exec(&bin.join("blender"), &fake_blender_wrapper());
    let path = format!("{}:/usr/bin:/bin", bin.display());
    let mut env = blender_env(&p, &[]);
    env.retain(|(k, _)| *k != "SITECRAFT_BLENDER");
    env.push(("SITECRAFT_BLENDER", os("")));
    env.push(("PATH", os(&path)));
    let (_o, v) = probe(&p, "blender", &env);
    assert_eq!(v["result"], "feasible", "{v}");
    assert!(
        p.marker("blender-ran").exists(),
        "an empty SITECRAFT_BLENDER must fall back to the PATH blender"
    );
}

#[test]
#[ignore = "only meaningful where /Applications/Blender.app is absent; this Mac has it"]
fn no_blender_anywhere_is_infeasible() {
    assert!(
        !Path::new("/Applications/Blender.app/Contents/MacOS/Blender").exists(),
        "precondition: no /Applications/Blender.app on this machine"
    );
    let p = blender_project();
    let mut env = blender_env(&p, &[]);
    env.retain(|(k, _)| *k != "SITECRAFT_BLENDER");
    env.push(("PATH", os("/usr/bin:/bin")));
    let (_o, v) = probe(&p, "blender", &env);
    assert_eq!(v["result"], "infeasible", "{v}");
}

#[test]
fn blender_nonzero_exit_is_infeasible() {
    let p = blender_project();
    let (_o, v) = probe(
        &p,
        "blender",
        &blender_env(&p, &[("FAKE_BLENDER_EXIT", "1")]),
    );
    assert_eq!(v["result"], "infeasible", "{v}");
}

#[test]
fn blender_output_failing_the_glb_condition_is_infeasible() {
    for mode in ["bad-magic", "empty", "none"] {
        let p = blender_project();
        let (_o, v) = probe(&p, "blender", &blender_env(&p, &[("FAKE_BPY_GLB", mode)]));
        assert_eq!(v["result"], "infeasible", "FAKE_BPY_GLB={mode}: {v}");
    }
}

// ---------------------------------------------------------------- three.js

fn threejs_project() -> Project {
    let p = Project::new();
    p.write_decisions(&format!(
        "{}{}",
        complete_record(),
        cap_threejs("threejs", "/viewer.html", None)
    ));
    p
}

fn threejs_env(p: &Project, scenario: Value) -> Vec<(&'static str, OsString)> {
    vec![
        ("SITECRAFT_NODE", os(which("node"))),
        ("FAKE_PLAYWRIGHT_SCENARIO", os(p.scenario(&scenario))),
        ("FAKE_PLAYWRIGHT_LOG", os(p.base.join("pw-log.jsonl"))),
    ]
}

#[test]
fn threejs_probe_with_webgl2_is_feasible() {
    let p = threejs_project();
    p.install_fake_playwright();
    let (_o, v) = probe(
        &p,
        "threejs",
        &threejs_env(&p, serde_json::json!({"webgl": "webgl2"})),
    );
    assert_eq!(v["result"], "feasible", "{v}");
    let log = jsonl(&p.base.join("pw-log.jsonl"));
    let launch = log
        .iter()
        .find(|e| e["op"] == "launch")
        .unwrap_or_else(|| panic!("Chromium must be launched: {log:?}"));
    assert_ne!(
        launch["opts"]["headless"],
        Value::Bool(false),
        "Chromium must be headless: {launch}"
    );
    // R3: the context is requested on `about:blank` (a new page starts
    // there; any explicit navigation must go to it and nowhere else).
    for g in log.iter().filter(|e| e["op"] == "goto") {
        assert_eq!(
            g["url"], "about:blank",
            "the three.js probe must only use about:blank: {g}"
        );
    }
    assert!(
        log.iter()
            .any(|e| e["op"] == "getContext" && e["type"] == "webgl2"),
        "the probe must ask a canvas for a webgl2 context: {log:?}"
    );
}

#[test]
fn threejs_probe_with_only_webgl_is_feasible() {
    let p = threejs_project();
    p.install_fake_playwright();
    let (_o, v) = probe(
        &p,
        "threejs",
        &threejs_env(&p, serde_json::json!({"webgl": "webgl"})),
    );
    assert_eq!(v["result"], "feasible", "{v}");
}

#[test]
fn threejs_probe_without_webgl_is_infeasible() {
    let p = threejs_project();
    p.install_fake_playwright();
    let (_o, v) = probe(
        &p,
        "threejs",
        &threejs_env(&p, serde_json::json!({"webgl": "none"})),
    );
    assert_eq!(v["result"], "infeasible", "{v}");
}

#[test]
fn threejs_probe_when_chromium_cannot_start_is_undetermined() {
    let p = threejs_project();
    p.install_fake_playwright();
    let (_o, v) = probe(
        &p,
        "threejs",
        &threejs_env(
            &p,
            serde_json::json!({"launch_error": "browserType.launch: Executable doesn't exist at /nowhere/chrome"}),
        ),
    );
    assert_eq!(v["result"], "undetermined", "{v}");
}

#[test]
fn threejs_probe_without_playwright_is_undetermined() {
    let p = threejs_project();
    // No node_modules/playwright in the root.
    let (_o, v) = probe(
        &p,
        "threejs",
        &threejs_env(&p, serde_json::json!({"webgl": "webgl2"})),
    );
    assert_eq!(v["result"], "undetermined", "{v}");
}

// ---------------------------------------------------------------- command

fn command_probe_project(argv0: &str) -> Project {
    let p = Project::new();
    // No sitecraft.toml and no trust at all: the probe must not need them.
    let marker = p.marker("tool-ran");
    write_exec(
        &p.path("tool.sh"),
        &format!(
            "#!/bin/sh\nmkdir -p '{}'\n: > '{}'\nexit 0\n",
            marker.parent().unwrap().display(),
            marker.display()
        ),
    );
    p.write("plain.txt", "not executable\n");
    p.write_decisions(&format!(
        "{}{}",
        complete_record(),
        cap_command("conv", &[argv0, "arg"], &["tool.sh"])
    ));
    p
}

#[test]
fn command_probe_resolves_argv0_without_running_it() {
    let cases: &[(&str, &str)] = &[
        ("./tool.sh", "feasible"),
        ("sh", "feasible"),
        ("./missing.sh", "infeasible"),
        ("./plain.txt", "infeasible"),
        ("definitely-not-a-command-sitecraft-spec", "infeasible"),
    ];
    for (argv0, want) in cases {
        let p = command_probe_project(argv0);
        let (_o, v) = probe(&p, "conv", &[]);
        assert_eq!(v["result"], *want, "argv[0] = {argv0:?}: {v}");
        assert!(
            !p.marker("tool-ran").exists(),
            "the probe must not execute the command (argv[0] = {argv0:?})"
        );
    }
    // Absolute path form.
    let p = command_probe_project("placeholder");
    let abs = p.path("tool.sh").display().to_string();
    p.write_decisions(&format!(
        "{}{}",
        complete_record(),
        cap_command("conv", &[&abs], &["tool.sh"])
    ));
    let (_o, v) = probe(&p, "conv", &[]);
    assert_eq!(v["result"], "feasible", "absolute argv[0]: {v}");
    assert!(
        !p.marker("tool-ran").exists(),
        "absolute argv[0] must not run"
    );
}

#[test]
fn probing_writes_neither_results_nor_decisions() {
    let p = blender_project();
    p.write_decisions(&format!(
        "{}{}{}",
        complete_record(),
        cap_blender("blender", "scripts/make.py", "out/m.glb", "out/r.png", &[]),
        cap_command("conv", &["sh"], &["x"])
    ));
    let before = p.decisions_bytes();
    let (_o, v) = probe(&p, "blender", &blender_env(&p, &[]));
    assert_eq!(v["result"], "feasible", "{v}");
    let (_o, v) = probe(&p, "conv", &[]);
    assert_eq!(v["result"], "feasible", "{v}");
    assert_eq!(
        p.decisions_bytes(),
        before,
        "probe must not touch decisions.toml"
    );
    assert!(
        !p.state.join("capability-results").exists(),
        "probe must not write capability-results"
    );
}

#[test]
fn skill_shows_probe_results_to_the_user() {
    assert_paragraph_with(
        &skill_text(),
        &["capability probe", "AskUserQuestion"],
        "R3: probe results are shown to the user with AskUserQuestion",
    );
}

#[test]
fn skill_says_feasible_is_not_completion() {
    assert_paragraph_with(
        &skill_text(),
        &["feasible", "完成"],
        "R3: probe feasible is not grounds for completion",
    );
}
