// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! False positive observed 2026-11-07 on main d9863f70 (condukt 0.7.331
//! installed): `condukt schedule` put task `fb338793` in `gated` with
//! `task 'fb338793' force-gated: high-risk irreversible action (declared class
//! Serial)` because its done_criteria PROSE contained the clause
//! "filter-branch filters stay Deny" — a task that only ADDS Deny rules and
//! never runs the command.
//!
//! Contracts in tension (quoted verbatim):
//! * `crates/condukt/skills/condukt/SKILL.md`: "classify は散文
//!   （title/done_criteria）をコマンドとして解析せず、タスクの `commands`
//!   （+ `reproduction_tests` / `checks`）だけをコマンドとして解析する
//!   （backlog dc955812）。"
//! * `crates/condukt/src/schedule.rs` `task_action_text` docstring: "its
//!   title, done-criteria, and touched files joined together, so a
//!   deploy/push signal in any of them trips the deterministic force-gate."
//!
//! The targets pin: prose that merely NAMES a destructive / history-rewrite
//! command, with no such declared command, is NOT force-gated. The controls
//! pin what must keep gating: a declared command that actually runs such an
//! action, and a prose deploy/push signal (the docstring's promise).

use std::path::PathBuf;
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

struct Fixture {
    dir: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.dir) {
            if e.kind() != std::io::ErrorKind::NotFound {
                eprintln!("WARNING: failed to remove {}: {e}", self.dir.display());
            }
        }
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let mut base = std::env::temp_dir();
        base.push(format!(
            "condukt-prose-fp-20261107-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        Self { dir: base }
    }
}

/// Run `condukt schedule` on a one-task decomposition and return
/// (gated ids, warnings). Fails the test on a non-zero exit or unparseable
/// output, so a crash is never read as "not gated".
fn schedule_one(
    tag: &str,
    title: &str,
    done_criteria: &str,
    commands: &[&str],
) -> (Vec<String>, Vec<String>) {
    let fx = Fixture::new(tag);
    let decomp = serde_json::json!({
        "goal": "g",
        "tasks": [{
            "id": "t1",
            "title": title,
            "touched_files": ["a.rs"],
            "deps": [],
            "class": "serial",
            "commands": commands,
            "done_criteria": done_criteria,
        }]
    });
    let p = fx.dir.join("decomp.json");
    std::fs::write(&p, serde_json::to_string(&decomp).unwrap()).unwrap();
    let out = harness_core::git_env::scrub(&mut Command::new(bin()))
        .arg("schedule")
        .arg("--file")
        .arg(&p)
        .output()
        .expect("spawn condukt schedule");
    assert!(
        out.status.success(),
        "condukt schedule must exit 0: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "schedule output must parse: {e}; stdout={}",
            String::from_utf8_lossy(&out.stdout)
        )
    });
    let strs = |k: &str| -> Vec<String> {
        v[k].as_array()
            .unwrap_or_else(|| panic!("schedule output lacks array `{k}`: {v}"))
            .iter()
            .map(|x| x.as_str().unwrap().to_string())
            .collect()
    };
    (strs("gated"), strs("warnings"))
}

// ---------------------------------------------------------------- targets

/// TARGET (RED at d9863f70): the exact minimal repro of the fb338793 force-gate.
#[test]
fn prose_naming_filter_branch_without_declared_command_is_not_gated() {
    let (gated, warnings) = schedule_one("fb", "x", "filter-branch filters stay Deny", &[]);
    assert!(
        gated.is_empty(),
        "prose that only NAMES filter-branch (commands: []) must not force-gate; \
         gated={gated:?} warnings={warnings:?}"
    );
}

/// TARGET (RED at d9863f70): the other history-rewrite names in prose
/// (`git rebase`, `rebase -i`, `--amend`) — a task that adds rules ABOUT them.
#[test]
fn prose_naming_history_rewrite_commands_without_declared_command_is_not_gated() {
    for (i, dc) in [
        "git rebase --exec shapes stay Deny",
        "rebase -i with an editor is Ask",
        "document that --amend rewrites the last commit",
    ]
    .iter()
    .enumerate()
    {
        let (gated, warnings) = schedule_one(&format!("hist{i}"), "x", dc, &[]);
        assert!(
            gated.is_empty(),
            "prose {dc:?} only names a command (commands: []) and must not force-gate; \
             gated={gated:?} warnings={warnings:?}"
        );
    }
}

/// Regression guard (passes at d9863f70): prose naming locally destructive
/// commands (`rm -rf`, `reset --hard`, `clean -fdx`) is not force-gated —
/// dc955812 already stopped handing prose to `detect`.
#[test]
fn prose_naming_destructive_commands_without_declared_command_is_not_gated() {
    for (i, (title, dc)) in [
        ("x", "rm -rf on a home path stays Deny"),
        ("make git reset --hard HEAD~1 Deny", "x"),
        ("x", "git clean -fdx is denied by the rule table"),
    ]
    .iter()
    .enumerate()
    {
        let (gated, warnings) = schedule_one(&format!("destr{i}"), title, dc, &[]);
        assert!(
            gated.is_empty(),
            "prose {title:?}/{dc:?} only names a command and must not force-gate; \
             gated={gated:?} warnings={warnings:?}"
        );
    }
}

// --------------------------------------------------------------- controls

/// CONTROL: a declared command that actually runs a destructive /
/// history-rewrite action is still force-gated.
#[test]
fn declared_destructive_or_history_rewrite_command_is_gated() {
    for (i, cmd) in [
        "rm -rf build",
        "git reset --hard HEAD~1",
        "git filter-branch --tree-filter true HEAD",
    ]
    .iter()
    .enumerate()
    {
        let (gated, warnings) = schedule_one(&format!("cmd{i}"), "x", "x", &[cmd]);
        assert_eq!(
            gated,
            vec!["t1".to_string()],
            "declared command {cmd:?} must force-gate; warnings={warnings:?}"
        );
    }
}

/// CONTROL: a deploy/push signal in prose still force-gates, exactly as the
/// `task_action_text` docstring promises ("so a deploy/push signal in any of
/// them trips the deterministic force-gate").
#[test]
fn prose_deploy_or_push_signal_still_force_gates() {
    for (i, (title, dc)) in [
        ("deploy to production", "x"),
        ("x", "git push origin main"),
        ("x", "publish the package to the registry"),
    ]
    .iter()
    .enumerate()
    {
        let (gated, warnings) = schedule_one(&format!("dep{i}"), title, dc, &[]);
        assert_eq!(
            gated,
            vec!["t1".to_string()],
            "prose deploy/push signal {title:?}/{dc:?} must force-gate; warnings={warnings:?}"
        );
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("force-gated: high-risk irreversible action")),
            "force-gate must be surfaced as a warning; warnings={warnings:?}"
        );
    }
}
