//! `local` — L1/L2/L3 over the four Claude Code settings files (CONTRACT.md §9).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(unix)]
mod common;
use common::*;
use serde_json::Value;

fn target_of<'a>(fs: &'a [Value], suffix: &str, rule: &str) -> &'a Value {
    let m: Vec<&Value> = fs
        .iter()
        .filter(|f| f["rule"] == rule && f["target"].as_str().is_some_and(|t| t.ends_with(suffix)))
        .collect();
    assert_eq!(m.len(), 1, "want one {rule} finding for *{suffix}: {fs:?}");
    m[0]
}

const HOME_S: &str = "home/.claude/settings.json";
const HOME_L: &str = "home/.claude/settings.local.json";
const PROJ_S: &str = "proj/.claude/settings.json";
const PROJ_L: &str = "proj/.claude/settings.local.json";

#[test]
fn l1_broad_bash_allow_is_violation() {
    for entry in ["Bash", "Bash(*)", "Bash(:*)", "*"] {
        let env = Env::new();
        write(
            &env.home.join(".claude/settings.json"),
            &serde_json::json!({"permissions": {"allow": ["Read", entry]}}).to_string(),
        );
        let o = env.run(&["local", "--json"]);
        assert_eq!(o.code, 1, "{entry}: {}", o.dbg());
        let fs = o.findings();
        let f = target_of(&fs, HOME_S, "L1");
        assert_eq!(f["verdict"], "violation", "{entry}: {f}");
        assert!(evidence_text(f).contains(entry), "{entry}: {f}");
        assert!(f["fix"].as_str().is_some_and(|s| !s.is_empty()), "{f}");
    }
}

#[test]
fn l1_narrow_bash_allow_is_clean() {
    let env = Env::new();
    write(
        &env.home.join(".claude/settings.json"),
        r#"{"permissions":{"allow":["Bash(git status:*)","Read"]}}"#,
    );
    let o = env.run(&["local", "--json"]);
    assert_eq!(o.code, 0, "{}", o.dbg());
    let fs = o.findings();
    assert_eq!(target_of(&fs, HOME_S, "L1")["verdict"], "clean");
    assert_eq!(target_of(&fs, HOME_S, "L2")["verdict"], "clean");
    assert_eq!(target_of(&fs, HOME_S, "L3")["verdict"], "clean");
}

#[test]
fn l2_bypass_permissions_is_violation() {
    let env = Env::new();
    write(
        &env.cwd.join(".claude/settings.local.json"),
        r#"{"permissions":{"defaultMode":"bypassPermissions"}}"#,
    );
    let o = env.run(&["local", "--json"]);
    assert_eq!(o.code, 1, "{}", o.dbg());
    let fs = o.findings();
    assert_eq!(target_of(&fs, PROJ_L, "L2")["verdict"], "violation");
    assert_eq!(target_of(&fs, PROJ_L, "L1")["verdict"], "clean");
}

#[test]
fn l3_skip_dangerous_prompt_is_violation() {
    let env = Env::new();
    write(
        &env.home.join(".claude/settings.local.json"),
        r#"{"skipDangerousModePermissionPrompt":true}"#,
    );
    let o = env.run(&["local", "--json"]);
    assert_eq!(o.code, 1, "{}", o.dbg());
    assert_eq!(
        target_of(&o.findings(), HOME_L, "L3")["verdict"],
        "violation"
    );
}

#[test]
fn absent_files_are_not_applicable_and_exit_zero() {
    let env = Env::new();
    let o = env.run(&["local", "--json"]);
    assert_eq!(o.code, 0, "{}", o.dbg());
    let fs = o.findings();
    assert_eq!(fs.len(), 12, "4 files x 3 rules: {fs:?}");
    for suf in [HOME_S, HOME_L, PROJ_S, PROJ_L] {
        for r in ["L1", "L2", "L3"] {
            assert_eq!(
                target_of(&fs, suf, r)["verdict"],
                "not_applicable",
                "{suf} {r}"
            );
        }
    }
}

#[test]
fn invalid_json_is_undetermined() {
    let env = Env::new();
    write(
        &env.cwd.join(".claude/settings.json"),
        "{ \"permissions\": ",
    );
    let o = env.run(&["local", "--json"]);
    assert_eq!(o.code, 1, "{}", o.dbg());
    let fs = o.findings();
    for r in ["L1", "L2", "L3"] {
        let f = target_of(&fs, PROJ_S, r);
        assert_eq!(f["verdict"], "undetermined", "{r}: {f}");
        assert!(f["reason"].as_str().is_some_and(|s| !s.is_empty()), "{f}");
    }
    assert_eq!(target_of(&fs, HOME_S, "L1")["verdict"], "not_applicable");
}

#[test]
fn unreadable_settings_is_undetermined() {
    let env = Env::new();
    std::fs::create_dir_all(env.home.join(".claude/settings.json")).unwrap();
    let o = env.run(&["local", "--json"]);
    assert_eq!(o.code, 1, "{}", o.dbg());
    assert_eq!(
        target_of(&o.findings(), HOME_S, "L1")["verdict"],
        "undetermined"
    );
}

#[test]
fn env_values_are_redacted_in_output_and_report() {
    let env = Env::new();
    write(
        &env.home.join(".claude/settings.json"),
        r#"{"env":{"MY_API_TOKEN":"hunter2-short-secret"},"permissions":{"allow":["Bash"]}}"#,
    );
    let o = env.run(&["local", "--json"]);
    assert_eq!(o.code, 1, "{}", o.dbg());
    assert_eq!(
        target_of(&o.findings(), HOME_S, "L1")["verdict"],
        "violation"
    );
    let rows = env.reports();
    assert!(
        rows.iter().any(|r| r["rule"] == "L1"),
        "local must append report rows"
    );
    for (what, s) in [
        ("stdout", &o.stdout),
        ("stderr", &o.stderr),
        ("report", &env.reports_raw()),
    ] {
        assert!(
            !s.contains("hunter2-short-secret"),
            "env value leaked in {what}: {s}"
        );
    }
}
