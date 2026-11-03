//! Spec `sitecraft-capabilities` R2 — registering capabilities (rulings U1,
//! D2, D2-trust, C4, D8).
//!
//! # CLI surface assumed
//!
//! * `sitecraft --root <root> decide capability --name N --relevance relevant|irrelevant
//!   --how T --done-when T --verify-kind K [per-kind args]` (D2 verbatim) writes
//!   `[capabilities.N]` with `relevance`, `how`, `done_when` and a `verify` table.
//!   Per-kind args (names chosen by the test author; the rulings leave them open):
//!   - `builtin:blender-headless`: `--script <rel>` `--glb <rel>` `--render <rel>`
//!     → `verify.script` / `verify.glb` / `verify.render` (strings);
//!     optional `--input <glob>` (repeatable) → `verify.inputs` (array).
//!   - `builtin:threejs-render`: `--page </path>` [`--selector <css>`]
//!     → `verify.page` / `verify.selector`; optional `--input` as above.
//!   - `command`: `--argv <s>` (repeatable, in order) → `verify.argv` (array),
//!     `--input <glob>` (repeatable) → `verify.inputs` (array).
//!
//!   `verify.kind` holds the `--verify-kind` value verbatim.
//! * A name not matching `^[a-z0-9][a-z0-9-]*$` → non-zero exit, decisions.toml
//!   byte-identical.
//! * `sitecraft decide check` (record otherwise complete): unknown kind,
//!   missing required key, or type mismatch in `verify` → exit 2; a relevant
//!   capability without `how` / `done_when` / `verify` → exit 1; no
//!   `[capabilities]` table → no capability finding at all (exit 0, output
//!   never mentions "capabilit").
//! * `sitecraft trust` (D2-trust): with >= 1 `command` capability, the trust
//!   store `<state>/trust.json` (a JSON object) gets key
//!   `<canonical root>#capabilities` = lowercase sha256 hex of
//!   `serde_json::to_string` (compact JSON) of the array `[[name, argv], …]`
//!   sorted by name, `command` capabilities only. Zero `command`
//!   capabilities → the key is absent and trust still succeeds.
//! * `sitecraft --root <root> check capability <name>` for a `command`
//!   capability whose argv changed since trust → exit 2 without running it,
//!   output contains `sitecraft trust`. Unreadable trust store → exit 2.
//!   Changes of `how` / `done_when` / builtin `verify` keep the trust.
//! * Re-recording a capability with a different value appends one
//!   `[[history]]` row `area = "capability"`, `question = N` (D8).
//! * SKILL.md: a paragraph with `how`, `done_when` and `AskUserQuestion`; a
//!   paragraph stating the LLM does not run `sitecraft trust` on the user's
//!   behalf (`LLM`, `sitecraft trust`, `代わりに`).
//!
//! Already-true guard: `skill_says_llm_does_not_run_trust`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod spec_common;

use spec_common::*;

fn verify_of(p: &Project, name: &str) -> toml::Value {
    let d = p.decisions();
    d.get("capabilities")
        .and_then(|c| c.get(name))
        .and_then(|c| c.get("verify"))
        .cloned()
        .unwrap_or_else(|| panic!("no [capabilities.{name}.verify] in {d:?}"))
}

fn cap_field(p: &Project, name: &str, key: &str) -> Option<String> {
    p.decisions()
        .get("capabilities")
        .and_then(|c| c.get(name))
        .and_then(|c| c.get(key))
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

fn strs(v: &toml::Value) -> Vec<String> {
    v.as_array()
        .unwrap_or_else(|| panic!("expected an array, got {v:?}"))
        .iter()
        .map(|s| s.as_str().expect("string element").to_string())
        .collect()
}

#[test]
fn decide_capability_records_a_blender_headless_capability() {
    let p = Project::new();
    p.write_decisions(&complete_record());
    let o = p.run(&[
        "decide",
        "capability",
        "--name",
        "blender-headless",
        "--relevance",
        "relevant",
        "--how",
        "Run a Blender script headless.",
        "--done-when",
        "A GLB and a drawn render are produced.",
        "--verify-kind",
        "builtin:blender-headless",
        "--script",
        "scripts/make.py",
        "--glb",
        "out/model.glb",
        "--render",
        "out/render.png",
    ]);
    expect_code(&o, 0, "decide capability (blender-headless)");
    assert_eq!(
        cap_field(&p, "blender-headless", "relevance").as_deref(),
        Some("relevant")
    );
    assert_eq!(
        cap_field(&p, "blender-headless", "how").as_deref(),
        Some("Run a Blender script headless.")
    );
    assert_eq!(
        cap_field(&p, "blender-headless", "done_when").as_deref(),
        Some("A GLB and a drawn render are produced.")
    );
    let v = verify_of(&p, "blender-headless");
    assert_eq!(v["kind"].as_str(), Some("builtin:blender-headless"));
    assert_eq!(v["script"].as_str(), Some("scripts/make.py"));
    assert_eq!(v["glb"].as_str(), Some("out/model.glb"));
    assert_eq!(v["render"].as_str(), Some("out/render.png"));
}

#[test]
fn decide_capability_records_a_threejs_render_capability() {
    let p = Project::new();
    p.write_decisions(&complete_record());
    let o = p.run(&[
        "decide",
        "capability",
        "--name",
        "threejs-render",
        "--relevance",
        "relevant",
        "--how",
        "A three.js viewer page.",
        "--done-when",
        "The canvas shows the model.",
        "--verify-kind",
        "builtin:threejs-render",
        "--page",
        "/viewer.html",
        "--selector",
        "#scene",
    ]);
    expect_code(&o, 0, "decide capability (threejs-render)");
    let v = verify_of(&p, "threejs-render");
    assert_eq!(v["kind"].as_str(), Some("builtin:threejs-render"));
    assert_eq!(v["page"].as_str(), Some("/viewer.html"));
    assert_eq!(v["selector"].as_str(), Some("#scene"));
}

#[test]
fn decide_capability_records_a_command_capability() {
    let p = Project::new();
    p.write_decisions(&complete_record());
    let o = p.run(&[
        "decide",
        "capability",
        "--name",
        "glb-to-vrm",
        "--relevance",
        "relevant",
        "--how",
        "A converter CLI.",
        "--done-when",
        "The converter exits 0.",
        "--verify-kind",
        "command",
        "--argv",
        "./tools/convert.sh",
        "--argv",
        "in.glb",
        "--input",
        "tools/**",
        "--input",
        "in.glb",
    ]);
    expect_code(&o, 0, "decide capability (command)");
    let v = verify_of(&p, "glb-to-vrm");
    assert_eq!(v["kind"].as_str(), Some("command"));
    assert_eq!(strs(&v["argv"]), vec!["./tools/convert.sh", "in.glb"]);
    assert_eq!(strs(&v["inputs"]), vec!["tools/**", "in.glb"]);
}

#[test]
fn invalid_capability_names_are_not_recorded() {
    let p = Project::new();
    p.write_decisions(&complete_record());
    for bad in [
        "Blender",
        "-lead",
        "under_score",
        "has space",
        "",
        "dot.name",
    ] {
        let before = p.decisions_bytes();
        let o = p.run(&[
            "decide",
            "capability",
            "--name",
            bad,
            "--relevance",
            "relevant",
            "--how",
            "h",
            "--done-when",
            "d",
            "--verify-kind",
            "command",
            "--argv",
            "./x.sh",
            "--input",
            "x.sh",
        ]);
        expect_failure(&o, &format!("decide capability --name {bad:?}"));
        assert_eq!(
            p.decisions_bytes(),
            before,
            "name {bad:?} must not change decisions.toml"
        );
    }
    // Positive control: a valid name in the same root is recorded.
    let ok = p.run(&[
        "decide",
        "capability",
        "--name",
        "ok-name1",
        "--relevance",
        "relevant",
        "--how",
        "h",
        "--done-when",
        "d",
        "--verify-kind",
        "command",
        "--argv",
        "./x.sh",
        "--input",
        "x.sh",
    ]);
    expect_code(&ok, 0, "decide capability --name ok-name1");
}

/// `[capabilities.x]` header + relevance/how/done_when, then `verify` body.
fn cap_with_verify(verify: &str) -> String {
    format!(
        "[capabilities.x]\nrelevance = \"relevant\"\nhow = \"h\"\ndone_when = \"d\"\n{verify}\n"
    )
}

#[test]
fn valid_capabilities_of_each_kind_check_clean() {
    for (what, cap) in [
        (
            "blender",
            cap_blender("b", "scripts/make.py", "out/m.glb", "out/r.png", &[]),
        ),
        ("threejs", cap_threejs("t", "/viewer.html", None)),
        ("command", cap_command("c", &["./x.sh", "a"], &["x.sh"])),
    ] {
        let p = Project::new();
        p.write_decisions(&format!("{}{cap}", complete_record()));
        expect_code(
            &p.run(&["decide", "check"]),
            0,
            &format!("decide check with a valid {what} capability"),
        );
    }
}

#[test]
fn unusable_verify_tables_make_decide_check_exit_2() {
    let cases: &[(&str, &str)] = &[
        (
            "unknown kind",
            "[capabilities.x.verify]\nkind = \"builtin:unity\"\nscript = \"a.py\"\n",
        ),
        ("missing kind", "[capabilities.x.verify]\nargv = [\"./a\"]\ninputs = [\"a\"]\n"),
        (
            "blender without script",
            "[capabilities.x.verify]\nkind = \"builtin:blender-headless\"\nglb = \"o.glb\"\nrender = \"o.png\"\n",
        ),
        (
            "blender without glb",
            "[capabilities.x.verify]\nkind = \"builtin:blender-headless\"\nscript = \"a.py\"\nrender = \"o.png\"\n",
        ),
        (
            "blender without render",
            "[capabilities.x.verify]\nkind = \"builtin:blender-headless\"\nscript = \"a.py\"\nglb = \"o.glb\"\n",
        ),
        (
            "blender script not a string",
            "[capabilities.x.verify]\nkind = \"builtin:blender-headless\"\nscript = 5\nglb = \"o.glb\"\nrender = \"o.png\"\n",
        ),
        ("threejs without page", "[capabilities.x.verify]\nkind = \"builtin:threejs-render\"\n"),
        (
            "threejs page not a string",
            "[capabilities.x.verify]\nkind = \"builtin:threejs-render\"\npage = [\"/v\"]\n",
        ),
        (
            "command without argv",
            "[capabilities.x.verify]\nkind = \"command\"\ninputs = [\"a\"]\n",
        ),
        (
            "command argv a string",
            "[capabilities.x.verify]\nkind = \"command\"\nargv = \"./a\"\ninputs = [\"a\"]\n",
        ),
        (
            "command argv empty",
            "[capabilities.x.verify]\nkind = \"command\"\nargv = []\ninputs = [\"a\"]\n",
        ),
        (
            "command argv[0] blank",
            "[capabilities.x.verify]\nkind = \"command\"\nargv = [\"  \", \"a\"]\ninputs = [\"a\"]\n",
        ),
        (
            "command argv non-string element",
            "[capabilities.x.verify]\nkind = \"command\"\nargv = [\"./a\", 3]\ninputs = [\"a\"]\n",
        ),
        (
            "command without inputs",
            "[capabilities.x.verify]\nkind = \"command\"\nargv = [\"./a\"]\n",
        ),
        (
            "command inputs empty",
            "[capabilities.x.verify]\nkind = \"command\"\nargv = [\"./a\"]\ninputs = []\n",
        ),
        (
            "command inputs a string",
            "[capabilities.x.verify]\nkind = \"command\"\nargv = [\"./a\"]\ninputs = \"a\"\n",
        ),
        ("verify not a table", "verify = \"command\"\n"),
    ];
    let valid = "[capabilities.x.verify]\nkind = \"command\"\nargv = [\"./a\"]\ninputs = [\"a\"]\n";
    for (what, verify) in cases {
        let p = Project::new();
        // Positive control in the same root: the usable table is clean, so
        // the exit 2 below comes from the broken verify, not from the
        // presence of [capabilities].
        p.write_decisions(&format!("{}{}", complete_record(), cap_with_verify(valid)));
        expect_code(
            &p.run(&["decide", "check"]),
            0,
            &format!("positive control before {what}"),
        );
        p.write_decisions(&format!("{}{}", complete_record(), cap_with_verify(verify)));
        let o = p.run(&["decide", "check"]);
        expect_code(&o, 2, &format!("decide check with {what}"));
    }
}

#[test]
fn relevant_capability_missing_how_done_when_or_verify_is_exit_1() {
    let verify =
        "[capabilities.x.verify]\nkind = \"command\"\nargv = [\"./a\"]\ninputs = [\"a\"]\n";
    let cases = [
        (
            "missing how",
            format!("[capabilities.x]\nrelevance = \"relevant\"\ndone_when = \"d\"\n{verify}"),
        ),
        (
            "missing done_when",
            format!("[capabilities.x]\nrelevance = \"relevant\"\nhow = \"h\"\n{verify}"),
        ),
        (
            "missing verify",
            "[capabilities.x]\nrelevance = \"relevant\"\nhow = \"h\"\ndone_when = \"d\"\n"
                .to_string(),
        ),
    ];
    for (what, cap) in cases {
        let p = Project::new();
        p.write_decisions(&format!("{}{cap}\n", complete_record()));
        let o = p.run(&["decide", "check"]);
        expect_code(&o, 1, &format!("decide check, relevant capability {what}"));
    }
}

#[test]
fn project_without_capabilities_table_gets_no_capability_findings() {
    let p = Project::new();
    p.write_decisions(&complete_record());
    let o = p.run(&["decide", "check"]);
    expect_code(&o, 0, "decide check without [capabilities]");
    assert!(
        !o.all().to_lowercase().contains("capabilit"),
        "no capability finding may be printed for a project without [capabilities]\n{}",
        o.describe()
    );
}

/// A project with `sitecraft.toml`, a complete record plus `caps`, and an
/// executable `tool.sh` in the root that writes its cwd to `marker`.
fn command_project(caps: &str) -> Project {
    let p = Project::new();
    p.write_sitecraft_toml("");
    let marker = p.marker("tool-ran");
    write_exec(
        &p.path("tool.sh"),
        &format!(
            "#!/bin/sh\nmkdir -p '{d}'\npwd -P > '{m}'\nexit 0\n",
            d = marker.parent().unwrap().display(),
            m = marker.display()
        ),
    );
    p.write_decisions(&format!("{}{caps}", complete_record()));
    p
}

fn tool_argv(p: &Project) -> String {
    p.path("tool.sh").display().to_string()
}

#[test]
fn trust_records_the_command_argv_digest_under_the_capabilities_key() {
    let p = Project::new();
    p.write_sitecraft_toml("");
    // Out of name order on purpose; a builtin capability that must not count.
    let caps = format!(
        "{}{}{}",
        cap_command("b-conv", &["./b.sh", "--fast"], &["b.sh"]),
        cap_blender("blender", "make.py", "o.glb", "o.png", &[]),
        cap_command("a-conv", &["./a.sh"], &["a.sh"]),
    );
    p.write_decisions(&format!("{}{caps}", complete_record()));
    p.trust_ok();
    let store = p.trust_store();
    let key = format!("{}#capabilities", p.canonical_root());
    let want = sha256_hex(
        serde_json::to_string(&serde_json::json!([
            ["a-conv", ["./a.sh"]],
            ["b-conv", ["./b.sh", "--fast"]]
        ]))
        .unwrap()
        .as_bytes(),
    );
    assert_eq!(
        store.get(&key).and_then(|v| v.as_str()),
        Some(want.as_str()),
        "trust.json[{key:?}] must be sha256 of the name-sorted [[name, argv], …] JSON; store: {store:?}"
    );
    assert!(
        store.contains_key(&p.canonical_root()),
        "the sitecraft.toml trust record stays under the plain root key: {store:?}"
    );
}

#[test]
fn trust_without_command_capabilities_does_not_write_the_capabilities_key() {
    let p = Project::new();
    p.write_sitecraft_toml("");
    p.write_decisions(&format!(
        "{}{}",
        complete_record(),
        cap_blender("blender", "make.py", "o.glb", "o.png", &[])
    ));
    p.trust_ok();
    let store = p.trust_store();
    assert!(
        store.contains_key(&p.canonical_root()),
        "sitecraft.toml must be trusted: {store:?}"
    );
    assert!(
        !store.keys().any(|k| k.ends_with("#capabilities")),
        "no `#capabilities` key without command capabilities: {store:?}"
    );
    // Positive control: adding a command capability and trusting again
    // writes the key, so its absence above is about the zero-command case.
    p.write_decisions(&format!(
        "{}{}{}",
        complete_record(),
        cap_blender("blender", "make.py", "o.glb", "o.png", &[]),
        cap_command("conv", &["./tool.sh"], &["tool.sh"])
    ));
    p.trust_ok();
    let store = p.trust_store();
    assert!(
        store.contains_key(&format!("{}#capabilities", p.canonical_root())),
        "with a command capability the `#capabilities` key must be written: {store:?}"
    );
}

#[test]
fn changed_argv_is_not_run_until_trusted_again() {
    let p = command_project("");
    let argv0 = tool_argv(&p);
    p.write_decisions(&format!(
        "{}{}",
        complete_record(),
        cap_command("conv", &[&argv0], &["tool.sh"])
    ));
    p.trust_ok();
    let marker = p.marker("tool-ran");
    // Positive control: trusted argv runs.
    let o = p.run(&["check", "capability", "conv"]);
    expect_code(&o, 0, "check capability conv (trusted)");
    assert!(marker.exists(), "the trusted command must have run");
    std::fs::remove_file(&marker).unwrap();

    p.write_decisions(&format!(
        "{}{}",
        complete_record(),
        cap_command("conv", &[&argv0, "--changed"], &["tool.sh"])
    ));
    let o = p.run(&["check", "capability", "conv"]);
    expect_code(&o, 2, "check capability conv after its argv changed");
    assert!(
        o.all().contains("sitecraft trust"),
        "the exit 2 must say to run `sitecraft trust`\n{}",
        o.describe()
    );
    assert!(!marker.exists(), "an untrusted argv must not be executed");
}

#[test]
fn unreadable_trust_store_is_exit_2_without_running_the_command() {
    let p = command_project("");
    let argv0 = tool_argv(&p);
    p.write_decisions(&format!(
        "{}{}",
        complete_record(),
        cap_command("conv", &[&argv0], &["tool.sh"])
    ));
    p.trust_ok();
    let marker = p.marker("tool-ran");
    expect_code(
        &p.run(&["check", "capability", "conv"]),
        0,
        "positive control: trusted command",
    );
    std::fs::remove_file(&marker).unwrap();
    write(&p.state.join("trust.json"), "{ this is not json");
    let o = p.run(&["check", "capability", "conv"]);
    expect_code(&o, 2, "check capability with an unreadable trust store");
    assert!(!marker.exists(), "nothing may run without readable trust");
}

#[test]
fn how_done_when_and_builtin_verify_changes_keep_the_trust() {
    let p = command_project("");
    let argv0 = tool_argv(&p);
    let cmd = cap_command("conv", &[&argv0], &["tool.sh"]);
    p.write_decisions(&format!(
        "{}{cmd}{}",
        complete_record(),
        cap_blender("blender", "make.py", "o.glb", "o.png", &[])
    ));
    p.trust_ok();
    let changed = cmd
        .replace(
            "how = \"a converter script\"",
            "how = \"a rewritten converter\"",
        )
        .replace(
            "done_when = \"the converter exits 0\"",
            "done_when = \"the converter really exits 0\"",
        );
    assert_ne!(
        changed, cmd,
        "setup: the how/done_when replacement must apply"
    );
    p.write_decisions(&format!(
        "{}{changed}{}",
        complete_record(),
        cap_blender("blender", "other.py", "o2.glb", "o2.png", &["assets/**"])
    ));
    let o = p.run(&["check", "capability", "conv"]);
    expect_code(
        &o,
        0,
        "check capability conv after only how/done_when/builtin verify changed",
    );
    assert!(p.marker("tool-ran").exists(), "the command must have run");
}

#[test]
fn rerecording_a_changed_capability_appends_a_history_row() {
    let p = Project::new();
    p.write_decisions(&complete_record());
    let rec = |how: &str| {
        p.run(&[
            "decide",
            "capability",
            "--name",
            "conv",
            "--relevance",
            "relevant",
            "--how",
            how,
            "--done-when",
            "d",
            "--verify-kind",
            "command",
            "--argv",
            "./x.sh",
            "--input",
            "x.sh",
        ])
    };
    expect_code(&rec("first how"), 0, "first decide capability");
    let before = p.history().len();
    expect_code(&rec("second how"), 0, "second decide capability");
    let h = p.history();
    assert_eq!(h.len(), before + 1, "one [[history]] row per change: {h:?}");
    let last = h.last().unwrap();
    assert_eq!(last["area"].as_str(), Some("capability"), "{last:?}");
    assert_eq!(last["question"].as_str(), Some("conv"), "{last:?}");
}

#[test]
fn skill_has_the_ask_step_for_how_and_done_when() {
    assert_paragraph_with(
        &skill_text(),
        &["how", "done_when", "AskUserQuestion"],
        "R2: capability how / done_when are decided by the user through AskUserQuestion",
    );
}

#[test]
fn skill_says_llm_does_not_run_trust() {
    assert_paragraph_with(
        &skill_text(),
        &["LLM", "sitecraft trust", "代わりに"],
        "R2: the LLM does not run `sitecraft trust` on the user's behalf",
    );
}
