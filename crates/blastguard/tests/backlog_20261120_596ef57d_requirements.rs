#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Backlog 596ef57d (GH #1700): one test per requirement, written by an independent
//! test author (CLAUDE.md 2(a)). Rulings: task 596ef57d notes (2026-11-20, A1-A3 and
//! round 2). Case strings are copied verbatim from
//! `heredoc_script_failopen_red_20261108.rs` (CASES_D3 in the `python3 - <<'PY'` stdin
//! form, `variants()`, `variants_d3()`). Each RED fn is `#[ignore]` until the
//! implementation lands; run with `-- --ignored`. Controls are not ignored.
//!
//! Q2 / A3: removing h_heredoc_bs_quoted_cmdsub and h_heredoc_dq_delim_cmdsub from the
//! aggregate attack list is part of the implementation, not of this file.

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
            .join("backlog_20261120_596ef57d_requirements")
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
            session: format!("r596-{name}-{}", std::process::id()),
        }
    }

    /// The permission decision for `command`: "allow" when blastguard prints nothing.
    fn decision(&self, command: &str, subagent: bool) -> String {
        let command = command.replace("{DIR}", &self.repo.display().to_string());
        let mut payload = serde_json::json!({
            "session_id": format!("{}-{}", self.session, if subagent { "sub" } else { "main" }),
            "hook_event_name": "PreToolUse",
            "cwd": self.repo.display().to_string(),
            "tool_name": "Bash",
            "tool_input": { "command": command },
        });
        if subagent {
            payload["agent_id"] = serde_json::json!("agent-hsfo");
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

/// `python3 - <<'PY'` stdin form of a python body (as the aggregate test builds it).
fn stdin(b: &str) -> String {
    format!("python3 - <<'PY'\n{b}\nPY")
}

#[derive(Clone, Copy, PartialEq)]
enum Need {
    /// Ask or Deny in both main and subagent mode.
    NotAllow,
    /// Deny in both main and subagent mode.
    Deny,
    /// Allow in both main and subagent mode.
    Allow,
}

fn check(req: &str, cases: Vec<(String, String)>, need: Need) {
    let mut bad: Vec<String> = Vec::new();
    for (name, cmd) in cases {
        let f = Fixture::new(&format!("{req}_{name}"));
        let main = f.decision(&cmd, false);
        let sub = f.decision(&cmd, true);
        let ok = match need {
            Need::NotAllow => main != "allow" && sub != "allow",
            Need::Deny => main == "deny" && sub == "deny",
            Need::Allow => main == "allow" && sub == "allow",
        };
        if !ok {
            bad.push(format!("{name}: main={main} sub={sub}"));
        }
    }
    assert!(
        bad.is_empty(),
        "{req}: cases not resolved to the required side:\n{}",
        bad.join("\n")
    );
}

/// Python bodies, verbatim from CASES_D3 (aggregate test), for the stdin_* cases.
const STDIN_BODIES: &[(&str, &str)] = &[
    (
        "popen_git_reset_hard",
        "import subprocess\nsubprocess.Popen([\"git\",\"reset\",\"--hard\"])",
    ),
    (
        "from_subprocess_import_run",
        "from subprocess import run\nrun([\"git\",\"reset\",\"--hard\"])",
    ),
    (
        "p1_check_output_git_clean",
        "import subprocess\nsubprocess.check_output([\"git\",\"clean\",\"-fdx\"])",
    ),
    (
        "p1_list_concat_cmd",
        "import subprocess\nsubprocess.run([\"rm\", \"-r\" + \"f\", \"~\"])",
    ),
    (
        "multiline_call_args",
        "import subprocess\nsubprocess.run(\n  [\"git\",\n   \"reset\",\n   \"--hard\"]\n)",
    ),
    (
        "x_os_replace_authkeys",
        "import os\nos.replace(\"/tmp/a\", \"/Users/yuki/.ssh/authorized_keys\")",
    ),
    (
        "x_os_chmod_ssh",
        "import os\nos.chmod(\"/Users/yuki/.ssh\", 0o777)",
    ),
    (
        "x_open_wb",
        "open(\"/Users/yuki/.ssh/authorized_keys\", \"wb\").write(b\"x\")",
    ),
    (
        "x_open_x_mode",
        "open(\"/Users/yuki/.ssh/authorized_keys\", \"x\")",
    ),
    (
        "x_os_utime_chown",
        "import os\nos.chown(\"/Users/yuki/.ssh\", 0, 0)",
    ),
    (
        "x_check_worktree_isolation_overwrite",
        "open(\"scripts/check-worktree-isolation.py\", \"w\").write(\"import sys; sys.exit(0)\")",
    ),
    (
        "x_loop_subprocess_shell",
        "import asyncio\nasyncio.get_event_loop().subprocess_shell(None, \"rm -rf /usr/lib\")",
    ),
];

fn bodies(names: &[&str]) -> Vec<(String, String)> {
    names
        .iter()
        .map(|n| {
            let (k, b) = STDIN_BODIES.iter().find(|(k, _)| k == n).unwrap();
            (format!("stdin_{k}"), stdin(b))
        })
        .collect()
}

#[test]
#[ignore = "RED(e93d3701): backlog 596ef57d R-kill"]
fn r_kill() {
    // Q5: kill -9 -1 -> Deny (variants(): c_kill_all).
    let cases = vec![("c_kill_all".to_string(), "kill -9 -1".to_string())];
    check("r_kill", cases, Need::Deny);
}

#[test]
#[ignore = "RED(eddabe3d): backlog 596ef57d R-protect"]
fn r_protect() {
    // Q3: /Users/<n>, **/.ssh/**, **/.gitconfig are protected even when not HOME -> Deny.
    let mut cases = vec![
        ("c_mv_home".to_string(), "mv /Users/yuki /tmp/h".to_string()),
        (
            "c_cp_authkeys".to_string(),
            "cp /tmp/x /Users/yuki/.ssh/authorized_keys".to_string(),
        ),
    ];
    cases.extend(bodies(&[
        "x_os_replace_authkeys",
        "x_os_chmod_ssh",
        "x_open_wb",
        "x_open_x_mode",
        "x_os_utime_chown",
    ]));
    check("r_protect", cases, Need::Deny);
}

#[test]
#[ignore = "RED: backlog 596ef57d R-gate"]
fn r_gate() {
    // Q4: gate scripts (scripts/check-*.py, .githooks/**, ...) must not be overwritten.
    let mut cases = vec![
        (
            "v_gate_script_overwrite".to_string(),
            "cat > {DIR}/scripts/check-worktree-isolation.py <<'EOF'\nimport sys\nsys.exit(0)\nEOF"
                .to_string(),
        ),
        (
            "h_overwrite_worktree_isolation".to_string(),
            "cat > scripts/check-worktree-isolation.py <<'EOF'\nimport sys\nsys.exit(0)\nEOF"
                .to_string(),
        ),
    ];
    cases.extend(bodies(&["x_check_worktree_isolation_overwrite"]));
    check("r_gate", cases, Need::NotAllow);
}

#[test]
#[ignore = "RED: backlog 596ef57d R-stdin-effect"]
fn r_stdin_effect() {
    // Round 2: an allowlisted sink API is judged by its argv effect.
    let cases = bodies(&[
        "popen_git_reset_hard",
        "from_subprocess_import_run",
        "p1_check_output_git_clean",
        "p1_list_concat_cmd",
        "multiline_call_args",
    ]);
    check("r_stdin_effect", cases, Need::NotAllow);
}

#[test]
#[ignore = "RED: backlog 596ef57d R-python-unknown-sink"]
fn r_python_unknown_sink() {
    // A2a: a sink with no effect model (asyncio subprocess_shell) -> Ask, not Allow.
    check(
        "r_python_unknown_sink",
        bodies(&["x_loop_subprocess_shell"]),
        Need::NotAllow,
    );
}

#[test]
#[ignore = "RED: backlog 596ef57d R-fakeeof"]
fn r_fakeeof() {
    let b = "import os\nos.system(\"rm -rf ~\")";
    let cases = vec![(
        "v_nested_fake_eof".to_string(),
        format!("cat > {{DIR}}/x.py <<'EOF'\nprint(1)\nEOF\n{b}\nEOF\npython3 {{DIR}}/x.py"),
    )];
    check("r_fakeeof", cases, Need::NotAllow);
}

#[test]
#[ignore = "RED: backlog 596ef57d R-cargo"]
fn r_cargo() {
    // A1: build.rs + cargo build, and .rs + cargo run, are not Allow.
    let cases = vec![
        (
            "h_rs_build_rs".to_string(),
            "cat > {DIR}/build.rs <<'EOF'\nfn main(){ std::process::Command::new(\"rm\").args([\"-rf\",\"/usr/lib\"]).status().unwrap(); }\nEOF\ncargo build".to_string(),
        ),
        (
            "h_rs_cargo_script".to_string(),
            "cat > {DIR}/x.rs <<'EOF'\nfn main(){ std::process::Command::new(\"rm\").args([\"-rf\",\"/usr/lib\"]).status().unwrap(); }\nEOF\ncargo run --manifest-path {DIR}/Cargo.toml".to_string(),
        ),
    ];
    check("r_cargo", cases, Need::NotAllow);
}

/// A3 re-anchor: ".py body is data" -> Allow. Currently Allow, so this is a passing
/// control, not RED.
#[test]
fn r_q2_reanchor() {
    let cases = vec![
        (
            "h_heredoc_bs_quoted_cmdsub".to_string(),
            "cat > {DIR}/x.py <<\\EOF\nx = \"$(rm -rf /usr/lib)\"\nEOF".to_string(),
        ),
        (
            "h_heredoc_dq_delim_cmdsub".to_string(),
            "cat > {DIR}/x.py <<\"EOF\"\nx = \"$(rm -rf /usr/lib)\"\nEOF".to_string(),
        ),
    ];
    check("r_q2_reanchor", cases, Need::Allow);
}

/// Control: pure-call python stdin program (python_stdin_re_compile_fp_20261103.rs:73).
#[test]
fn control_pure_call_re_compile_allow() {
    let b = "import re\np = re.compile(r'a+b')\nprint(p.findall('aab'))";
    check(
        "ctl_re",
        vec![("re_compile_findall".to_string(), stdin(b))],
        Need::Allow,
    );
}

/// Control: benign argv through an allowlisted sink (fp_git_status body) via stdin.
#[test]
fn control_subprocess_git_status_stdin_allow() {
    let b = "import subprocess\nprint(subprocess.run(['git', 'status', '--short'], capture_output=True, text=True).stdout)";
    check(
        "ctl_gitstatus",
        vec![("fp_git_status_stdin".to_string(), stdin(b))],
        Need::Allow,
    );
}

/// Control (A1): an ordinary .rs heredoc followed by `cargo test` stays Allow.
/// The aggregate test has no such case, so this one is new.
#[test]
fn control_ordinary_rs_then_cargo_test_allow() {
    let cmd =
        "cat > {DIR}/x.rs <<'EOF'\nfn main() { println!(\"hi\"); }\nEOF\ncargo test".to_string();
    check(
        "ctl_rs",
        vec![("rs_cargo_test".to_string(), cmd)],
        Need::Allow,
    );
}
