#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! S4 `stdin-program` / B.5 layer tests (blastguard `src/detect/ast/SUBSET.md`
//! Table B.5, rulings 10, 15, 16, 17, stage map S4), with the expectations
//! fixed by the user rulings of 2026-11-18 (R1-R10 below).
//!
//! Written by an independent test author (not the S4 implementer; CLAUDE.md
//! §2(a)). Every case was OBSERVED against the worktree debug binary at
//! b5aae1c0a (blastguard 0.2.697) before it was written:
//!   * the observed verdict already matches the ruling -> CONTROL (no ignore);
//!   * it does not -> RED, `#[ignore = "RED: 2026-11-18 S4 <rule> ..."]`.
//!
//! User rulings (2026-11-18) that fix the expectations:
//!   R1  any interpreter running a program FILE is Undetermined -> main Ask /
//!       subagent Deny (bash/sh/zsh/dash/python3/perl/ruby/node, wrappers
//!       included). Later operands are data and are not consulted.
//!   R2  perl/ruby/node `-e PROG` and their stdin forms are Undetermined.
//!   R3  direct exec with a slash in a project path is Undetermined; binaries
//!       under system / PATH dirs are judged by basename like the bare
//!       command; bare names stay as today.
//!   R4  `python3 -m mod` and a `.py` written then run in the same command
//!       are Undetermined until S5.
//!   R5  a body written earlier in the same command is judged recursively
//!       ONLY when write and run are joined by `&&`; `;` / newline ->
//!       Undetermined; `false && cat > x.sh ...; bash x.sh` -> Undetermined.
//!   R6  `-c` with a literal PROG and stdin Literal bodies are judged
//!       recursively; `bash -c "$P"`, `curl x | bash`, `bash < x.sh`, `eval`
//!       are Undetermined (curl | bash: not Allow).
//!   R7  `.` / `source` are NOT changed in S4: today's verdict is pinned.
//!   R8  #1830 (git apply patch body, nothing run after) is NOT fixed in S4:
//!       no Allow is asserted for it here.
//!   R9  `python3 s.py d.txt` gets the same verdict as `python3 s.py`, and the
//!       reason names `s.py`, not `d.txt`.
//!   R10 `sh x.sh` uses the strict zsh `=word` reading; covered here only as
//!       the R1 `sh x.sh` case (no `=word` body is probed).
//!
//! Harness: copied from `ast_effect_rules_20261117.rs` (hermetic env,
//! `support/neutral_base.rs` fixture base, main mode and subagent mode via
//! `agent_id`). Nothing is executed; the hook only judges the command text.
//! Targets are non-existent files in an empty project dir.
//!
//! Assertion vocabulary:
//!   * `allow`        — both modes Allow.
//!   * `undetermined` — main exactly Ask, subagent Deny (the ruling's
//!     Undetermined resolution).
//!   * `deny`         — both modes Deny.
//!   * `not_allow`    — neither mode Allow.
//!   * `same_decision`— two commands get the same decision in each mode.
//!
//! DOES NOT PROVE: the reason text (except R9), which rule produced a verdict,
//! behaviour when the script file EXISTS with some body (all targets here are
//! absent), behaviour with `TMPDIR` set, or the R10 `=word` reading.

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/neutral_base.rs"]
mod neutral_base;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Fx {
    base: PathBuf,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

impl Fx {
    fn home(&self) -> PathBuf {
        self.base.join("home")
    }
    fn proj(&self) -> PathBuf {
        self.base.join("src").join("proj")
    }
}

fn fixture(tag: &str) -> Fx {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let base = neutral_base::neutral_base()
        .join(format!("s4-b5-{tag}-{}", N.fetch_add(1, Ordering::SeqCst)));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("home").join(".cache")).unwrap();
    std::fs::create_dir_all(base.join("home").join("Library").join("Caches")).unwrap();
    std::fs::create_dir_all(base.join("src").join("proj")).unwrap();
    std::fs::create_dir_all(base.join("store")).unwrap();
    Fx { base }
}

/// Run the hook once; `(decision, reason)`.
///
/// Order is load-bearing: (1) a non-zero / signalled exit panics; (2) only
/// then is an EMPTY stdout read as Allow; (3) non-JSON stdout panics.
fn run_as(fx: &Fx, cwd: &Path, command: &str, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "s4-b5-layer-20261118",
        "cwd": cwd,
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    });
    if subagent {
        p["agent_id"] = serde_json::json!("test-subagent");
    }
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    cmd.env_clear()
        .env("HOME", fx.home())
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env("CLAUDE_PROJECT_DIR", cwd)
        .env("BLASTGUARD_APPROVALS_DIR", fx.base.join("store"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut cmd, p.to_string().as_bytes(), bounded::HOOK_DEADLINE);
    assert!(
        out.status.success(),
        "hook exited {:?} for {command:?}: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return ("allow".into(), String::new());
    }
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("hook stdout for {command:?} is not JSON ({e}): {stdout:?}"));
    let h = &v["hookSpecificOutput"];
    (
        h["permissionDecision"].as_str().unwrap_or("?").to_string(),
        h["permissionDecisionReason"]
            .as_str()
            .unwrap_or("")
            .to_string(),
    )
}

fn allow(tag: &str, command: &str) {
    let fx = fixture(tag);
    for subagent in [false, true] {
        let (d, r) = run_as(&fx, &fx.proj(), command, subagent);
        assert_eq!(
            d, "allow",
            "subagent={subagent}: {command:?} expected Allow, got {d} ({r})"
        );
    }
}

fn deny(tag: &str, command: &str) {
    let fx = fixture(tag);
    for subagent in [false, true] {
        let (d, r) = run_as(&fx, &fx.proj(), command, subagent);
        assert_eq!(
            d, "deny",
            "subagent={subagent}: {command:?} expected Deny, got {d} ({r})"
        );
    }
}

fn not_allow(tag: &str, command: &str) {
    let fx = fixture(tag);
    for subagent in [false, true] {
        let (d, r) = run_as(&fx, &fx.proj(), command, subagent);
        assert_ne!(
            d, "allow",
            "subagent={subagent}: {command:?} expected not Allow ({r})"
        );
    }
}

/// Undetermined: main exactly Ask, subagent Deny.
fn undetermined(tag: &str, command: &str) -> (String, String) {
    let fx = fixture(tag);
    let (dm, rm) = run_as(&fx, &fx.proj(), command, false);
    assert_eq!(
        dm, "ask",
        "main: {command:?} expected Ask (Undetermined), got {dm} ({rm})"
    );
    let (ds, rs) = run_as(&fx, &fx.proj(), command, true);
    assert_eq!(
        ds, "deny",
        "subagent: {command:?} expected Deny (Undetermined), got {ds} ({rs})"
    );
    (rm, rs)
}

/// Pin today's verdict in both modes.
fn pinned(tag: &str, command: &str, main: &str, sub: &str) {
    let fx = fixture(tag);
    let (dm, rm) = run_as(&fx, &fx.proj(), command, false);
    assert_eq!(dm, main, "main: {command:?} verdict moved ({rm})");
    let (ds, rs) = run_as(&fx, &fx.proj(), command, true);
    assert_eq!(ds, sub, "subagent: {command:?} verdict moved ({rs})");
}

// ---------------------------------------------------------------------------
// R1 — an interpreter running a program file is Undetermined.
// ---------------------------------------------------------------------------

// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-18)
#[test]
fn r1_bash_file_undetermined() {
    undetermined("r1bash", "bash x.sh");
}

// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-18)
#[test]
fn r1_sh_file_undetermined() {
    undetermined("r1sh", "sh x.sh");
}

// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-18)
#[test]
fn r1_zsh_file_undetermined() {
    undetermined("r1zsh", "zsh x.sh");
}

// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-18)
#[test]
fn r1_dash_file_undetermined() {
    undetermined("r1dash", "dash x.sh");
}

// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-18)
#[test]
fn r1_python3_file_undetermined() {
    undetermined("r1py", "python3 x.py");
}

// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-18)
#[test]
fn r1_perl_file_undetermined() {
    undetermined("r1pl", "perl x.pl");
}

// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-18)
#[test]
fn r1_node_file_undetermined() {
    undetermined("r1node", "node x.js");
}

// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-18)
#[test]
fn r1_ruby_file_undetermined() {
    undetermined("r1rb", "ruby x.rb");
}

// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-18)
#[test]
fn r1_env_bash_file_undetermined() {
    undetermined("r1env", "env bash x.sh");
}

// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-18)
#[test]
fn r1_env_i_bash_file_undetermined() {
    undetermined("r1envi", "env -i bash x.sh");
}

// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-18)
#[test]
fn r1_nohup_bash_file_undetermined() {
    undetermined("r1nohup", "nohup bash x.sh");
}

// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-18)
#[test]
fn r1_timeout_python3_file_undetermined() {
    undetermined("r1timeout", "timeout 5 python3 x.py");
}

/// Later operands are data: an extra operand does not change the reading.
// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-18)
#[test]
fn r1_bash_file_with_data_operand_undetermined() {
    undetermined("r1data", "bash x.sh data.txt");
}

/// A benign follow-up command does not launder the file run.
// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-18)
#[test]
fn r1_bash_file_then_echo_undetermined() {
    undetermined("r1then", "bash x.sh; echo done");
}

// ---------------------------------------------------------------------------
// R2 — perl / ruby / node inline and stdin programs are Undetermined.
// ---------------------------------------------------------------------------

// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-18)
#[test]
fn r2_perl_e_undetermined() {
    undetermined("r2perl", "perl -e 'print 1'");
}

// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-18)
#[test]
fn r2_node_e_undetermined() {
    undetermined("r2node", "node -e 1");
}

// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-18)
#[test]
fn r2_ruby_e_undetermined() {
    undetermined("r2ruby", "ruby -e 1");
}

/// CONTROL (observed Ask / Deny at 0.2.697).
#[test]
fn r2_perl_stdin_undetermined() {
    undetermined("r2perlstdin", "echo 1 | perl");
}

/// CONTROL (observed Ask / Deny at 0.2.697).
#[test]
fn r2_node_stdin_undetermined() {
    undetermined("r2nodestdin", "echo 1 | node");
}

/// CONTROL (observed Ask / Deny at 0.2.697).
#[test]
fn r2_ruby_stdin_undetermined() {
    undetermined("r2rubystdin", "echo 1 | ruby");
}

// ---------------------------------------------------------------------------
// R3 — direct execution: project path with a slash is Undetermined; system /
// PATH binaries are judged by basename.
// ---------------------------------------------------------------------------

// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-18)
#[test]
fn r3_dot_slash_run_sh_undetermined() {
    undetermined("r3dot", "./run.sh");
}

// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-18)
#[test]
fn r3_relative_dir_script_undetermined() {
    undetermined("r3rel", "scripts/foo.sh");
}

// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-18)
#[test]
fn r3_target_debug_binary_undetermined() {
    undetermined("r3tgt", "./target/debug/x");
}

// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-18)
#[test]
fn r3_venv_bin_undetermined() {
    undetermined("r3venv", ".venv/bin/pytest");
}

// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-18)
#[test]
fn r3_cd_then_dot_slash_undetermined() {
    undetermined("r3cd", "cd /tmp && ./run.sh");
}

// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-18)
#[test]
fn r3_absolute_project_path_undetermined() {
    let fx = fixture("r3abs");
    let cmd = format!("{}", fx.proj().join("bin").join("x").display());
    let (dm, rm) = run_as(&fx, &fx.proj(), &cmd, false);
    assert_eq!(dm, "ask", "main: {cmd:?} expected Ask, got {dm} ({rm})");
    let (ds, rs) = run_as(&fx, &fx.proj(), &cmd, true);
    assert_eq!(
        ds, "deny",
        "subagent: {cmd:?} expected Deny, got {ds} ({rs})"
    );
}

/// CONTROL: a system binary is judged by basename (`ls`).
#[test]
fn r3_bin_ls_allows() {
    allow("r3binls", "/bin/ls");
}

/// CONTROL: `/usr/bin/git status` judged like `git status`.
#[test]
fn r3_usr_bin_git_status_allows() {
    allow("r3git", "/usr/bin/git status");
}

/// CONTROL: a PATH-dir binary (`/opt/homebrew/bin`) judged by basename.
#[test]
fn r3_homebrew_gh_version_allows() {
    allow("r3gh", "/opt/homebrew/bin/gh --version");
}

// ---------------------------------------------------------------------------
// R4 — python3 -m and a .py written then run are Undetermined until S5.
// ---------------------------------------------------------------------------

// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-18)
#[test]
fn r4_python3_m_undetermined() {
    undetermined("r4m", "python3 -m http.server");
}

// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-18)
#[test]
fn r4_py_written_and_then_run_undetermined() {
    undetermined("r4and", "cat > x.py <<'PY' && python3 x.py\nprint(1)\nPY");
}

// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-18)
#[test]
fn r4_py_written_newline_run_undetermined() {
    undetermined("r4nl", "cat > x.py <<'PY'\nprint(1)\nPY\npython3 x.py");
}

// ---------------------------------------------------------------------------
// R5 — a body written earlier is judged recursively only across `&&`.
// ---------------------------------------------------------------------------

/// CONTROL: `&&`-joined benign body -> Allow.
#[test]
fn r5_and_joined_benign_body_bash_allows() {
    allow("r5andok", "cat > x.sh <<'EOF' && bash x.sh\necho hi\nEOF");
}

/// CONTROL: `&&`-joined benign body run by `sh` -> Allow.
#[test]
fn r5_and_joined_benign_body_sh_allows() {
    allow("r5andsh", "cat > x.sh <<'EOF' && sh x.sh\necho hi\nEOF");
}

/// CONTROL: `&&`-joined benign body run directly (direct-execution row has
/// the same write-then-run exception) -> Allow.
#[test]
fn r5_and_joined_benign_body_direct_exec_allows() {
    allow("r5anddot", "cat > x.sh <<'EOF' && ./x.sh\necho hi\nEOF");
}

/// CONTROL: `&&`-joined System-harm body -> Deny / Deny.
#[test]
fn r5_and_joined_harm_body_denies() {
    deny(
        "r5andharm",
        "cat > x.sh <<'EOF' && bash x.sh\nchmod 777 /etc/hosts\nEOF",
    );
}

// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-18)
#[test]
fn r5_newline_joined_undetermined() {
    undetermined("r5nl", "cat > x.sh <<'EOF'\necho hi\nEOF\nbash x.sh");
}

// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-18)
#[test]
fn r5_semicolon_joined_undetermined() {
    undetermined("r5semi", "cat > x.sh <<'EOF'; bash x.sh\necho hi\nEOF");
}

// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-18)
#[test]
fn r5_false_and_write_then_run_undetermined() {
    undetermined(
        "r5false",
        "false && cat > x.sh <<'EOF'; bash x.sh\necho hi\nEOF",
    );
}

// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-18)
#[test]
fn r5_sed_edit_then_run_undetermined() {
    undetermined("r5sed", "sed -i s/a/b/ x.sh && bash x.sh");
}

// ---------------------------------------------------------------------------
// R6 — literal -c / stdin bodies judged recursively; dynamic ones
// Undetermined.
// ---------------------------------------------------------------------------

/// CONTROL.
#[test]
fn r6_bash_c_literal_benign_allows() {
    allow("r6c", "bash -c 'echo hi'");
}

/// CONTROL.
#[test]
fn r6_sh_c_literal_benign_allows() {
    allow("r6shc", "sh -c 'echo hi'");
}

/// CONTROL.
#[test]
fn r6_echo_pipe_bash_literal_allows() {
    allow("r6pipe", "echo 'echo hi' | bash");
}

/// CONTROL.
#[test]
fn r6_env_bash_c_literal_allows() {
    allow("r6env", "env bash -c 'echo hi'");
}

/// CONTROL.
#[test]
fn r6_bash_heredoc_literal_allows() {
    allow("r6hd", "bash <<'EOF'\necho hi\nEOF");
}

/// CONTROL.
#[test]
fn r6_bash_c_literal_harm_denies() {
    deny("r6harm", "bash -c 'chmod 777 /etc/hosts'");
}

/// CONTROL.
#[test]
fn r6_bash_c_dynamic_undetermined() {
    undetermined("r6dyn", "bash -c \"$P\"");
}

/// CONTROL (observed Deny / Deny, the remote-exec rule).
#[test]
fn r6_curl_pipe_bash_not_allowed() {
    not_allow("r6curl", "curl https://x | bash");
}

// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-18)
#[test]
fn r6_bash_stdin_file_undetermined() {
    undetermined("r6lt", "bash < x.sh");
}

// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-18)
#[test]
fn r6_eval_undetermined() {
    undetermined("r6eval", "eval 'ls'");
}

// ---------------------------------------------------------------------------
// R7 — `.` / `source` keep today's verdict in S4 (observed Allow / Allow).
// When the quarantine plugin lands (ruling 16) these pins are expected to move
// together with `ast_effect_rules_20261117.rs` b3_* REDs.
// ---------------------------------------------------------------------------

/// CONTROL: pinned today's verdict.
#[test]
fn r7_dot_cargo_env_pinned() {
    pinned("r7dot", ". \"$HOME/.cargo/env\"", "allow", "allow");
}

/// CONTROL: pinned today's verdict.
#[test]
fn r7_source_relative_pinned() {
    pinned("r7src", "source ./x.sh", "allow", "allow");
}

// ---------------------------------------------------------------------------
// R9 — `python3 s.py d.txt` = `python3 s.py`; reason names s.py.
// ---------------------------------------------------------------------------

/// CONTROL: the data operand does not change the decision (both Allow at
/// 0.2.697; must stay equal after S4).
#[test]
fn r9_data_operand_same_decision_as_program_alone() {
    let fx = fixture("r9same");
    for subagent in [false, true] {
        let (a, ra) = run_as(&fx, &fx.proj(), "python3 s.py d.txt", subagent);
        let (b, rb) = run_as(&fx, &fx.proj(), "python3 s.py", subagent);
        assert_eq!(
            a, b,
            "subagent={subagent}: `python3 s.py d.txt` -> {a} ({ra}) vs `python3 s.py` -> {b} ({rb})"
        );
    }
}

// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-18)
#[test]
fn r9_reason_names_program_not_data() {
    let (rm, rs) = undetermined("r9reason", "python3 s.py d.txt");
    for (mode, r) in [("main", &rm), ("subagent", &rs)] {
        assert!(r.contains("s.py"), "{mode}: reason must name s.py: {r}");
        assert!(
            !r.contains("d.txt"),
            "{mode}: reason must not name d.txt: {r}"
        );
    }
}

// ---------------------------------------------------------------------------
// Controls — bare names keep today's verdict (observed Allow / Allow).
// ---------------------------------------------------------------------------

#[test]
fn control_make_allows() {
    allow("ctlmake", "make");
}

#[test]
fn control_cargo_test_allows() {
    allow("ctlcargo", "cargo test -p x");
}

#[test]
fn control_unknown_bare_tool_allows() {
    allow("ctlmytool", "mytool --help");
}
