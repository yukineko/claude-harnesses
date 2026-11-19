#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED: the Q3/Q4 deny-target path table (user rulings 2026-11-14).
//!
//! Q3: `/Users/<n>` and `/home/<n>` (the home directory itself), `**/.ssh/**`
//! and `**/.gitconfig` are Deny targets EVEN WHEN NOT under `$HOME`.
//! Q4: gate scripts (`.githooks/**`, `scripts/check-*.py`, `crates/*/hooks/**`,
//! `~/.claude/plugins/cache/**/{bin,hooks}`) must not be written, moved,
//! chmod-ed or deleted.
//!
//! The fixture HOME is a hermetic directory that is NOT any of the targeted
//! `/Users/<n>` paths, so the "even when not HOME" part is exercised.
//!
//! Cases that are Deny today are plain tests; every case OBSERVED not to be
//! Deny is `#[ignore = "RED(...)"]`. Controls assert the verdict observed
//! today (reads and substring look-alikes), so a change is visible.
//!
//! Harmful strings live only in these test sources, never in a shell command
//! that is executed: blastguard is only ever shown them as hook payload text.

#[path = "support/git_env.rs"]
mod git_env;
#[path = "support/neutral_base.rs"]
mod neutral_base;

#[path = "support/bounded.rs"]
mod bounded;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn git(dir: &Path, args: &[&str]) {
    let mut cmd = git_env::pinned(dir);
    cmd.args([
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@example.invalid",
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "init.defaultBranch=main",
    ])
    .args(args)
    .env("GIT_CONFIG_GLOBAL", "/dev/null")
    .env("GIT_CONFIG_NOSYSTEM", "1");
    let out =
        bounded::run_bounded_stdin(&mut cmd, bounded::ChildStdin::Null, bounded::TOOL_DEADLINE);
    assert!(out.status.success(), "git {args:?} failed");
}

struct Fixture {
    base: PathBuf,
    home: PathBuf,
    repo: PathBuf,
    session: String,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let base = neutral_base::neutral_base()
            .join("backlog_20261120_q3q4_deny_target_table")
            .join(name);
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        let repo = base.join("src/repo");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        std::fs::write(repo.join("f.txt"), "x\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        Fixture {
            base,
            home,
            repo,
            session: format!("q3q4-{name}-{}", std::process::id()),
        }
    }

    /// The permission decision for `command`: "allow" when blastguard prints nothing.
    fn decision(&self, command: &str, subagent: bool) -> String {
        let command = command
            .replace("{DIR}", &self.repo.display().to_string())
            .replace("{HOME}", &self.home.display().to_string())
            .replace("{BASE}", &self.base.display().to_string());
        let mut payload = serde_json::json!({
            "session_id": format!("{}-{}", self.session, if subagent { "sub" } else { "main" }),
            "hook_event_name": "PreToolUse",
            "cwd": self.repo.display().to_string(),
            "tool_name": "Bash",
            "tool_input": { "command": command },
        });
        if subagent {
            payload["agent_id"] = serde_json::json!("agent-q3q4");
            payload["agent_type"] = serde_json::json!("general-purpose");
        }
        let session = payload["session_id"].as_str().unwrap().to_string();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
        git_env::scrub(&mut cmd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("CLAUDE_PROJECT_DIR", &self.repo)
            .env("BLASTGUARD_APPROVALS_DIR", self.base.join("store"))
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_ENTRYPOINT", "cli")
            .env("CLAUDE_CODE_SESSION_ID", &session)
            .env_remove("BLASTGUARD_ASK")
            .env_remove("TMPDIR")
            .current_dir(&self.base);
        let out = bounded::run_bounded(
            &mut cmd,
            payload.to_string().as_bytes(),
            bounded::HOOK_DEADLINE,
        );
        let out = String::from_utf8_lossy(&out.stdout).into_owned();
        if out.trim().is_empty() {
            return "allow".to_string();
        }
        let v = serde_json::from_str::<serde_json::Value>(out.trim()).unwrap();
        v.pointer("/hookSpecificOutput/permissionDecision")
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_else(|| "nodecision".into())
    }
}

/// Deny-target paths. `{DIR}` = repo, `{HOME}` = the hermetic HOME (never
/// `/Users/somebody`), `{BASE}` = a neutral dir that is neither HOME nor repo.
const TARGETS: &[(&str, &str)] = &[
    ("ssh_authkeys_users", "/Users/somebody/.ssh/authorized_keys"),
    ("ssh_idrsa_anydir", "{BASE}/anydir/.ssh/id_rsa"),
    (
        "gate_check_script",
        "{DIR}/scripts/check-worktree-isolation.py",
    ),
    ("githooks_precommit", "{DIR}/.githooks/pre-commit"),
    ("crate_hooks", "{DIR}/crates/foo/hooks/hook.sh"),
    (
        "plugin_cache_bin",
        "{HOME}/.claude/plugins/cache/x/y/1.0/bin/tool",
    ),
    ("gitconfig_anydir", "{BASE}/anydir/.gitconfig"),
];

/// (op id, command template with `{T}` = target path)
const OPS: &[(&str, &str)] = &[
    ("redirect", "echo k > {T}"),
    ("mv", "mv /tmp/x {T}"),
    ("cp", "cp /tmp/x {T}"),
    ("chmod777", "chmod 777 {T}"),
    ("rm", "rm {T}"),
    ("ln_sf", "ln -sf /tmp/x {T}"),
];

/// The home directory itself (`/Users/<n>`, `/home/<n>`), not under HOME.
const HOME_DIR_CASES: &[(&str, &str)] = &[
    ("users_mv_away", "mv /Users/somebody /tmp/gone"),
    ("users_rm_rf", "rm -rf /Users/somebody"),
    ("users_chmod_777", "chmod -R 777 /Users/somebody"),
    ("users_ln_sf", "ln -sf /tmp/x /Users/somebody"),
    ("home_mv_away", "mv /home/somebody /tmp/gone"),
    ("home_rm_rf", "rm -rf /home/somebody"),
];

/// Inline python onto 3 of the targets.
const PY_CASES: &[(&str, &str)] = &[
    (
        "py_os_replace_users_authkeys",
        "python3 - <<'PY'\nimport os\nos.replace('/tmp/x', '/Users/somebody/.ssh/authorized_keys')\nPY",
    ),
    (
        "py_os_chmod_anydir_idrsa",
        "python3 - <<'PY'\nimport os\nos.chmod('{BASE}/anydir/.ssh/id_rsa', 0o777)\nPY",
    ),
    (
        "py_open_wb_gate_script",
        "python3 - <<'PY'\nopen('{DIR}/scripts/check-worktree-isolation.py', 'wb').write(b'import sys\\nsys.exit(0)\\n')\nPY",
    ),
];

/// Controls: not Q3/Q4 writes. (id, command, verdict observed today).
const CONTROLS: &[(&str, &str, &str)] = &[
    (
        "ctl_cat_authkeys",
        "cat /Users/somebody/.ssh/authorized_keys",
        "allow",
    ),
    (
        "ctl_cat_gate_script",
        "cat {DIR}/scripts/check-worktree-isolation.py",
        "allow",
    ),
    ("ctl_cat_gitconfig", "cat {BASE}/anydir/.gitconfig", "allow"),
    (
        "ctl_cat_precommit",
        "cat {DIR}/.githooks/pre-commit",
        "allow",
    ),
    (
        "ctl_py_open_r_authkeys",
        "python3 - <<'PY'\nprint(open('/Users/somebody/.ssh/authorized_keys', 'r').read())\nPY",
        "allow",
    ),
    (
        "ctl_write_tmp_scratch",
        "echo k > /tmp/q3q4_scratch_file",
        "allow",
    ),
    ("ctl_write_substring_sshx", "echo k > /tmp/sshx/f", "allow"),
    (
        "ctl_write_substring_notscripts",
        "echo k > /tmp/notscripts/check-x.txt",
        "allow",
    ),
];

fn all_deny_cases() -> Vec<(String, String)> {
    let mut v = Vec::new();
    for (tid, t) in TARGETS {
        for (oid, o) in OPS {
            v.push((format!("{tid}__{oid}"), o.replace("{T}", t)));
        }
    }
    for (id, c) in HOME_DIR_CASES.iter().chain(PY_CASES) {
        v.push((id.to_string(), c.to_string()));
    }
    v
}

const ALL_OPS: &[&str] = &["redirect", "mv", "cp", "chmod777", "rm", "ln_sf"];

fn target_ids(target: &str, ops: &[&str]) -> Vec<String> {
    ops.iter().map(|o| format!("{target}__{o}")).collect()
}

/// Every id must be Deny for both the main session and a subagent (Ask is
/// not Deny; Allow is the fail-open). All failures are listed, not just the
/// first.
fn assert_all_deny(ids: &[String]) {
    let cases = all_deny_cases();
    let mut bad = Vec::new();
    for id in ids {
        let cmd = &cases
            .iter()
            .find(|(i, _)| i == id)
            .unwrap_or_else(|| panic!("no case {id}"))
            .1;
        let fx = Fixture::new(id);
        let (m, s) = (fx.decision(cmd, false), fx.decision(cmd, true));
        if m != "deny" || s != "deny" {
            bad.push(format!("{id}: main={m} sub={s}  [{cmd}]"));
        }
    }
    assert!(
        bad.is_empty(),
        "want deny/deny, not met for {} case(s):\n{}",
        bad.len(),
        bad.join("\n")
    );
}

fn ids(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

// ---- RED: observed main=allow sub=allow on rev 84d930e83 ----

#[test]
#[ignore = "RED(7275b9d9): .ssh/authorized_keys under /Users/<n> (not HOME) write/mv/cp/chmod/rm/ln are allow"]
fn red_ssh_authkeys_users_all_ops() {
    assert_all_deny(&target_ids("ssh_authkeys_users", ALL_OPS));
}

#[test]
#[ignore = "RED(7275b9d9): <anydir>/.ssh/id_rsa write/mv/cp/chmod/rm/ln are allow"]
fn red_ssh_idrsa_anydir_all_ops() {
    assert_all_deny(&target_ids("ssh_idrsa_anydir", ALL_OPS));
}

#[test]
#[ignore = "RED(7275b9d9): scripts/check-*.py write/mv/cp/chmod/rm/ln are allow"]
fn red_gate_check_script_all_ops() {
    assert_all_deny(&target_ids("gate_check_script", ALL_OPS));
}

#[test]
#[ignore = "RED(7275b9d9): crates/*/hooks/** write/mv/cp/chmod/rm/ln are allow"]
fn red_crate_hooks_all_ops() {
    assert_all_deny(&target_ids("crate_hooks", ALL_OPS));
}

#[test]
#[ignore = "RED(7275b9d9): <anydir>/.gitconfig write/mv/cp/chmod/rm/ln are allow"]
fn red_gitconfig_anydir_all_ops() {
    assert_all_deny(&target_ids("gitconfig_anydir", ALL_OPS));
}

#[test]
#[ignore = "RED(7275b9d9): chmod 777 on .githooks/** is allow (observed main=allow sub=allow)"]
fn red_githooks_chmod777() {
    assert_all_deny(&target_ids("githooks_precommit", &["chmod777"]));
}

#[test]
#[ignore = "RED(7275b9d9): chmod 777 on ~/.claude/plugins/cache/**/bin is allow (observed main=allow sub=allow)"]
fn red_plugin_cache_chmod777() {
    assert_all_deny(&target_ids("plugin_cache_bin", &["chmod777"]));
}

#[test]
#[ignore = "RED(7275b9d9): mv/ln -sf of the home directory itself (/Users/<n>, /home/<n>) is allow"]
fn red_home_dir_mv_and_ln() {
    assert_all_deny(&ids(&["users_mv_away", "users_ln_sf", "home_mv_away"]));
}

#[test]
#[ignore = "RED(7275b9d9): python os.replace onto /Users/<n>/.ssh/authorized_keys is allow"]
fn red_py_os_replace_users_authkeys() {
    assert_all_deny(&ids(&["py_os_replace_users_authkeys"]));
}

#[test]
#[ignore = "RED(7275b9d9): python os.chmod on <anydir>/.ssh/id_rsa is allow"]
fn red_py_os_chmod_anydir_idrsa() {
    assert_all_deny(&ids(&["py_os_chmod_anydir_idrsa"]));
}

#[test]
#[ignore = "RED(7275b9d9): python open(...,'wb') on scripts/check-*.py is allow"]
fn red_py_open_wb_gate_script() {
    assert_all_deny(&ids(&["py_open_wb_gate_script"]));
}

// ---- RED: observed main=ask sub=deny (Q4 ruling says Deny) ----

#[test]
#[ignore = "RED(7275b9d9): write/mv/cp/ln -sf onto .githooks/** is main=ask sub=deny; ruling is Deny"]
fn red_githooks_write_ops_main_ask() {
    assert_all_deny(&target_ids(
        "githooks_precommit",
        &["redirect", "mv", "cp", "ln_sf"],
    ));
}

#[test]
#[ignore = "RED(7275b9d9): write/mv/cp/ln -sf onto ~/.claude/plugins/cache/**/bin is main=ask sub=deny; ruling is Deny"]
fn red_plugin_cache_write_ops_main_ask() {
    assert_all_deny(&target_ids(
        "plugin_cache_bin",
        &["redirect", "mv", "cp", "ln_sf"],
    ));
}

// ---- GREEN: already Deny today (regression guard) ----

#[test]
fn already_deny_rm_of_githooks_plugin_cache_and_home_dir() {
    assert_all_deny(&[
        "githooks_precommit__rm".to_string(),
        "plugin_cache_bin__rm".to_string(),
        "users_rm_rf".to_string(),
        "users_chmod_777".to_string(),
        "home_rm_rf".to_string(),
    ]);
}

// ---- Controls: verdict observed today, asserted as-is ----

#[test]
fn controls_keep_current_verdict() {
    let mut bad = Vec::new();
    for (id, cmd, want) in CONTROLS {
        let fx = Fixture::new(id);
        let (m, s) = (fx.decision(cmd, false), fx.decision(cmd, true));
        if m != *want || s != *want {
            bad.push(format!("{id}: want {want}, got main={m} sub={s}"));
        }
    }
    assert!(
        bad.is_empty(),
        "control verdict changed:\n{}",
        bad.join("\n")
    );
}
