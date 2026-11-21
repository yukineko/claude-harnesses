#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Adversarial probes for backlog 8cadc78f (an `echo` feeder whose output is a
//! shell program that removes `/etc`, piped to bash, was Allow; system-harming
//! targets are Deny in every shape/wrapper). Every probe runs in main and
//! subagent mode. Written by an independent test author, not the implementer.
//!
//! DOES NOT PROVE: shapes not listed here, nor anything about non-Bash tools.

#[path = "support/bounded.rs"]
mod bounded;

use std::process::{Command, Stdio};

struct Home(std::path::PathBuf);
impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn unique_home(tag: &str) -> Home {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "bg-8cadc78f-adv-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn decide(command: &str, home: &Home, subagent: bool) -> String {
    let mut p = serde_json::json!({
        "session_id": "backlog-8cadc78f-adversarial-20261120",
        "cwd": home.0.join("wt"),
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    });
    if subagent {
        p["agent_id"] = serde_json::json!("test-subagent");
    }
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    cmd.env_clear()
        .env("HOME", &home.0)
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut cmd, p.to_string().as_bytes(), bounded::HOOK_DEADLINE);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return "allow".into();
    }
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    v["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .unwrap_or("?")
        .to_string()
}

/// `want`: "deny" | "not_allow" | "allow" | "not_deny"
fn check(cmd: &str, tag: &str, want: &str, property: &str) {
    let home = unique_home(tag);
    let mut bad = Vec::new();
    let mut seen = Vec::new();
    for subagent in [false, true] {
        let d = decide(cmd, &home, subagent);
        seen.push(format!("subagent={subagent}:{d}"));
        let ok = match want {
            "deny" => d == "deny",
            "not_allow" => d == "ask" || d == "deny",
            "allow" => d == "allow",
            "not_deny" => d != "deny",
            _ => false,
        };
        if !ok {
            bad.push(format!("subagent={subagent} => {d}"));
        }
    }
    assert!(
        bad.is_empty(),
        "property [{property}] expected {want} for {cmd:?}; observed {}:\n{}",
        seen.join(" "),
        bad.join("\n")
    );
}

#[test]
fn p01_sh() {
    check(
        r####"echo rm -rf /etc | sh"####,
        "p01",
        "deny",
        "system harm through sh",
    );
}

#[test]
fn p02_bash_s() {
    check(
        r####"echo rm -rf /etc | bash -s"####,
        "p02",
        "deny",
        "bash -s",
    );
}

#[test]
fn p03_quoted() {
    check(
        r####"echo 'rm -rf /etc' | bash"####,
        "p03",
        "deny",
        "quoted feeder",
    );
}

#[test]
fn p04_dash() {
    check(r####"echo rm -rf /etc | dash"####, "p04", "deny", "dash");
}

#[test]
fn p05_echo_e() {
    check(
        r####"echo -e 'rm -rf /etc' | bash"####,
        "p05",
        "deny",
        "echo -e",
    );
}

#[test]
fn p06_echo_n() {
    check(
        r####"echo -n rm -rf /etc | bash"####,
        "p06",
        "deny",
        "echo -n",
    );
}

#[test]
fn p07_env_bash() {
    check(
        r####"echo rm -rf /etc | env bash"####,
        "p07",
        "deny",
        "env wrapper",
    );
}

#[test]
fn p08_sudo_bash() {
    check(
        r####"echo rm -rf /etc | sudo bash"####,
        "p08",
        "not_allow",
        "sudo wrapper",
    );
}

#[test]
fn p09_bash_c_nested() {
    check(
        r####"bash -c "echo rm -rf /etc | bash""####,
        "p09",
        "deny",
        "bash -c wrapper",
    );
}

#[test]
fn p10_double_nest() {
    check(
        r####"echo "echo rm -rf /etc | bash" | bash"####,
        "p10",
        "deny",
        "two-level dquote nesting",
    );
}

#[test]
fn p10b_original() {
    check(
        r####"echo 'echo rm -rf /etc | bash' | bash"####,
        "p10b",
        "deny",
        "original backlog command",
    );
}

#[test]
fn p11_three_level() {
    check(
        r####"echo "echo 'echo rm -rf /etc | bash' | bash" | bash"####,
        "p11",
        "deny",
        "three-level nesting",
    );
}

#[test]
fn p12_brace_sink() {
    check(
        r####"echo rm -rf /etc | { bash; }"####,
        "p12",
        "deny",
        "brace-group sink",
    );
}

#[test]
fn p13_usr() {
    check(
        r####"echo rm -rf /usr | bash"####,
        "p13",
        "deny",
        "/usr target",
    );
}

#[test]
fn p14_home() {
    check(r####"echo rm -rf ~ | bash"####, "p14", "deny", "~ target");
}

#[test]
fn p15_backslash_space() {
    check(
        r####"echo rm\ -rf\ /etc | bash"####,
        "p15",
        "deny",
        "escaped spaces",
    );
}

#[test]
fn p16_empty_quotes() {
    check(
        r####"echo rm -rf /e""tc | bash"####,
        "p16",
        "deny",
        "empty-quote split path",
    );
}

#[test]
fn p17_var() {
    check(
        r####"X=/etc; echo rm -rf $X | bash"####,
        "p17",
        "deny",
        "variable path assigned earlier",
    );
}

#[test]
fn p18_redirect_stdout() {
    check(
        r####"echo rm -rf /etc > /dev/stdout | bash"####,
        "p18",
        "deny",
        "echo redirected to /dev/stdout",
    );
}

#[test]
fn p19_redefine_func() {
    check(
        r####"echo() { :; }; echo rm -rf /etc | bash"####,
        "p19",
        "not_allow",
        "echo function redefinition",
    );
}

#[test]
fn p20_alias() {
    check(
        r####"alias echo='printf %s'; echo rm -rf /etc | bash"####,
        "p20",
        "not_allow",
        "echo alias",
    );
}

#[test]
fn p21_semicolon_prog() {
    check(
        r####"echo 'true; rm -rf /etc' | bash"####,
        "p21",
        "deny",
        "multiple commands in program",
    );
}

#[test]
fn p22_and_prog() {
    check(
        r####"echo true && echo rm -rf /etc | bash"####,
        "p22",
        "deny",
        "&& before feeder",
    );
}

#[test]
fn p23_bash_c_in_program() {
    check(
        r####"echo "bash -c 'rm -rf /etc'" | bash"####,
        "p23",
        "deny",
        "bash -c inside echo program",
    );
}

#[test]
fn p24_sh_c_wrapper() {
    check(
        r####"sh -c 'echo rm -rf /etc | sh'"####,
        "p24",
        "deny",
        "sh -c wrapper then pipe",
    );
}

#[test]
fn p25_cmdsubst() {
    check(
        r####"echo "$(echo rm -rf /etc)" | bash"####,
        "p25",
        "deny",
        "command substitution in feeder program",
    );
}

#[test]
fn p26_printf() {
    check(
        r####"printf 'rm -rf /etc\n' | bash"####,
        "p26",
        "deny",
        "printf feeder",
    );
}

#[test]
fn p27_root() {
    check(
        r####"echo rm -rf / | bash"####,
        "p27",
        "deny",
        "root target",
    );
}

#[test]
fn p28_dash_stdin() {
    check(
        r####"echo rm -rf /etc | bash -"####,
        "p28",
        "deny",
        "bash - (stdin)",
    );
}

#[test]
fn p29_here_string() {
    check(
        r####"bash <<< 'rm -rf /etc'"####,
        "p29",
        "deny",
        "here-string to bash",
    );
}

#[test]
fn p30_dev_stdin() {
    check(
        r####"echo rm -rf /etc | bash /dev/stdin"####,
        "p30",
        "deny",
        "bash /dev/stdin",
    );
}

#[test]
fn p31_cat_hop() {
    check(
        r####"echo rm -rf /etc | cat | bash"####,
        "p31",
        "deny",
        "two-hop feeder via cat",
    );
}

#[test]
fn p32_tee_hop() {
    check(
        r####"echo rm -rf /etc | tee /dev/null | bash"####,
        "p32",
        "deny",
        "tee passthrough",
    );
}

#[test]
fn p33_escaped_semicolon() {
    check(
        r####"echo rm -rf /etc \; true | bash"####,
        "p33",
        "deny",
        "escaped semicolon unquoted",
    );
}

#[test]
fn p34_command_echo() {
    check(
        r####"command echo rm -rf /etc | bash"####,
        "p34",
        "deny",
        "command echo builtin bypass",
    );
}

#[test]
fn p35_builtin_echo() {
    check(
        r####"builtin echo rm -rf /etc | bash"####,
        "p35",
        "deny",
        "builtin echo",
    );
}

#[test]
fn p36_env_echo() {
    check(
        r####"env echo rm -rf /etc | bash"####,
        "p36",
        "deny",
        "env echo as feeder",
    );
}

#[test]
fn p37_abs_echo() {
    check(
        r####"/bin/echo rm -rf /etc | bash"####,
        "p37",
        "deny",
        "absolute path echo",
    );
}

#[test]
fn p38_ansi_c_quote() {
    check(
        r####"echo $'rm -rf /etc' | bash"####,
        "p38",
        "deny",
        "ANSI-C quoted program",
    );
}

#[test]
fn p39_echo_cap_e() {
    check(
        r####"echo -E 'rm -rf /etc' | bash"####,
        "p39",
        "deny",
        "echo -E",
    );
}

#[test]
fn p40_sudo_rm() {
    check(
        r####"echo sudo rm -rf /etc | bash"####,
        "p40",
        "deny",
        "sudo rm in program",
    );
}

// Nested forms of the variants that were Ask in main when un-nested.
#[test]
fn n01_env_bash() {
    check(
        r####"echo 'echo rm -rf /etc | env bash' | bash"####,
        "n01",
        "deny",
        "nested env bash sink",
    );
}

#[test]
fn n02_backslash_space() {
    check(
        r####"echo 'echo rm\ -rf\ /etc | bash' | bash"####,
        "n02",
        "deny",
        "nested escaped spaces",
    );
}

#[test]
fn n03_var() {
    check(
        r####"echo 'X=/etc; echo rm -rf $X | bash' | bash"####,
        "n03",
        "deny",
        "nested variable path",
    );
}

#[test]
fn n04_redirect_stdout() {
    check(
        r####"echo 'echo rm -rf /etc > /dev/stdout | bash' | bash"####,
        "n04",
        "deny",
        "nested /dev/stdout redirect",
    );
}

#[test]
fn n05_escaped_semicolon() {
    check(
        r####"echo 'echo rm -rf /etc \; true | bash' | bash"####,
        "n05",
        "deny",
        "nested escaped semicolon",
    );
}

#[test]
fn n06_abs_echo() {
    check(
        r####"echo '/bin/echo rm -rf /etc | bash' | bash"####,
        "n06",
        "deny",
        "nested absolute-path echo",
    );
}

// Controls (no new false positives).
#[test]
#[ignore = "backlog 7d85ed1e RED: 7d85ed1e-family FP: echo hi | bash is Ask in main, Deny in subagent"]
fn c01_echo_hi() {
    check(
        r####"echo hi | bash"####,
        "c01",
        "allow",
        "pinned: echo hi | bash is Allow (ast_eval_fp_targets L224)",
    );
}

#[test]
fn c02_quoted_hi() {
    check(
        r####"echo 'echo hi' | bash"####,
        "c02",
        "allow",
        "pinned: quoted feeder Allow",
    );
}

#[test]
fn c03_brace_group() {
    check(
        r####"{ echo a; echo b; } | bash"####,
        "c03",
        "allow",
        "pinned: brace-group feeder Allow",
    );
}

#[test]
#[ignore = "backlog 7d85ed1e RED: 7d85ed1e-family FP: echo ls | bash is Ask in main, Deny in subagent"]
fn c04_echo_ls() {
    check(
        r####"echo ls | bash"####,
        "c04",
        "not_deny",
        "harmless program not Deny",
    );
}

#[test]
#[ignore = "backlog 7d85ed1e RED: 7d85ed1e-family FP: nested echo ls | bash is Ask in main, Deny in subagent"]
fn c05_nested_harmless() {
    check(
        r####"echo 'echo ls | bash' | bash"####,
        "c05",
        "not_deny",
        "harmless nested not Deny",
    );
}

#[test]
#[ignore = "backlog 987e3708 RED: possible FP: rm of ./build via echo|bash is Deny in main and subagent"]
fn c06_rm_build() {
    check(
        r####"echo rm -rf ./build | bash"####,
        "c06",
        "not_deny",
        "non-system rm target not Deny",
    );
}
