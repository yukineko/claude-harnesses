#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Adversarial tests (disinterested author, CLAUDE.md section 2(a)) for
//! detect.rs step 0c, `heredoc::non_shell_code_file_here_document`: a QUOTED
//! here-document body written by `cat`/`tee` only to literal non-shell code
//! files (`.rs`, `.go`, `.js`, ...) is not judged as shell when every later
//! line is read-only.
//!
//! The body of every hostile case is `rm -rf ~` (Deny if judged as shell), so
//! an Allow can only come from step 0c dropping it. Each hostile case asserts
//! the verdict is NOT allow: either the body must stay judged, or something
//! outside the body must make the command restrictive. Verdicts only (empty
//! stdout = allow), never message text. Two controls pin the intended allow
//! and that the body really is judged as shell without the relaxation.
//!
//! Cases are grouped: later lines that run/source/build the file, tools on the
//! inert list that execute things, extra or shell-ish output targets, tricky
//! delimiters, CRLF, shebangs, unicode, symlinked targets.

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
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let base = neutral_base::neutral_base()
            .join("non_shell_code_heredoc_failopen_20261111")
            .join(format!("{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        let repo = base.join("src/repo");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(repo.join("src")).unwrap();
        git(&repo, &["init", "-q"]);
        std::fs::write(repo.join("f.txt"), "x\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        // Symlink fixtures: .rs-named links onto files a body must never reach.
        std::fs::write(home.join(".zshrc"), "# rc\n").unwrap();
        std::fs::write(repo.join(".git/hooks/pre-commit"), "#!/bin/sh\n").unwrap();
        std::os::unix::fs::symlink(home.join(".zshrc"), repo.join("link.rs")).unwrap();
        std::os::unix::fs::symlink(repo.join(".git/hooks/pre-commit"), repo.join("hook.rs"))
            .unwrap();
        std::os::unix::fs::symlink("/etc/hosts", repo.join("etc.rs")).unwrap();
        std::os::unix::fs::symlink(home.join(".bashrc"), repo.join("dang.rs")).unwrap();
        std::os::unix::fs::symlink(home.join(".zshrc"), repo.join("link.js")).unwrap();
        Fixture { base, home, repo }
    }

    fn decision(&self, command: &str) -> (String, String) {
        let command = command
            .replace("{DIR}", &self.repo.display().to_string())
            .replace("{HOME}", &self.home.display().to_string());
        let session = format!("nscfo-{}", std::process::id());
        let payload = serde_json::json!({
            "session_id": session,
            "hook_event_name": "PreToolUse",
            "cwd": self.repo.display().to_string(),
            "tool_name": "Bash",
            "tool_input": { "command": command },
        });
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
        assert_eq!(
            out.status.code(),
            Some(0),
            "hook must exit 0 for {command:?}"
        );
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        if stdout.trim().is_empty() {
            return ("allow".into(), String::new());
        }
        let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
        let d = v
            .pointer("/hookSpecificOutput/permissionDecision")
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_else(|| "nodecision".into());
        (d, stdout)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

/// `cat > {DIR}/src/x.rs <<'EOF'` + body `rm -rf ~` + `EOF`, then `tail`.
fn rs(tail: &str) -> String {
    format!("cat > {{DIR}}/src/x.rs <<'EOF'\nrm -rf ~\nEOF\n{tail}")
}

macro_rules! hostile {
    ($name:ident, $cmd:expr) => {
        #[test]
        fn $name() {
            let fx = Fixture::new(stringify!($name));
            let cmd: String = $cmd;
            let (d, out) = fx.decision(&cmd);
            assert_ne!(
                d,
                "allow",
                "{}: FAIL-OPEN, must be ask/deny; command {cmd:?}; decision JSON: {out:?}",
                stringify!($name)
            );
        }
    };
}

// ---- controls ---------------------------------------------------------------

/// Intended allow: body is Rust (with a lone backtick and a destructive-looking
/// comment), later line is read-only.
#[test]
fn control_rs_body_with_read_only_tail_allows() {
    let fx = Fixture::new("ctl-allow");
    let (d, out) = fx.decision(&rs("ls {DIR}/src\n"));
    assert_eq!(d, "allow", "intended allow; got {out:?}");
}

/// Same body to a `.sh` target is judged as shell, so is not allowed: proves
/// the hostile bodies below would be caught if step 0c did not drop them.
#[test]
fn control_sh_target_same_body_is_not_allowed() {
    let fx = Fixture::new("ctl-sh");
    let (d, out) = fx.decision("cat > {DIR}/src/x.sh <<'EOF'\nrm -rf ~\nEOF\nls {DIR}/src\n");
    assert_ne!(d, "allow", "body to .sh must stay judged; got {out:?}");
}

// ---- later lines that run, source or build the file --------------------------

hostile!(later_sh, rs("sh {DIR}/src/x.rs\n"));
hostile!(later_bash, rs("bash {DIR}/src/x.rs\n"));
hostile!(later_dot, rs(". {DIR}/src/x.rs\n"));
hostile!(later_source, rs("source {DIR}/src/x.rs\n"));
hostile!(later_exec_path, rs("{DIR}/src/x.rs\n"));
hostile!(
    later_chmod_then_run,
    rs("chmod +x {DIR}/src/x.rs && {DIR}/src/x.rs\n")
);
hostile!(later_env_sh, rs("env sh {DIR}/src/x.rs\n"));
hostile!(later_time_sh, rs("time sh {DIR}/src/x.rs\n"));
hostile!(later_command_sh, rs("command sh {DIR}/src/x.rs\n"));
hostile!(later_xargs, rs("echo {DIR}/src/x.rs | xargs sh\n"));
hostile!(later_rustc, rs("rustc {DIR}/src/x.rs\n"));
// Re-anchored to user ruling 2026-11-18 (B-4), not deleted. This case used to
// pin `rs("cargo test\n")` (write `.rs`, then `cargo test`) as not-Allow; the
// ruling makes that exact command Allow (building is not running the body),
// and `ruling_b4_cargo_test_after_rs_write_harmful_body_allows` below pins it
// Allow. What the ruling keeps restricted is running the written file with a
// shell after the build, so the hostile case now pins THAT: `cargo test`
// followed by `sh` on the same file stays not-Allow. (A `hostile!`
// invocation can only assert not-Allow and scripts/check-test-weakening.py
// cannot prove an invocation's removal safe, so the name is kept and its
// command moved to the ruling's restricted side.)
hostile!(later_cargo_test, rs("cargo test && sh {DIR}/src/x.rs\n"));

/// Ruling B-4, benign-looking body: write `.rs`, then `cargo test` => Allow
/// (already Allow at origin/main 04d9b8336).
#[test]
fn ruling_b4_cargo_test_after_rs_write_benign_body_allows() {
    let fx = Fixture::new("b4-benign");
    let cmd = "cat > {DIR}/src/x.rs <<'EOF'\nfn main() {}\nEOF\ncargo test\n";
    let (d, out) = fx.decision(cmd);
    assert_eq!(d, "allow", "ruling B-4; command {cmd:?}; got {out:?}");
}

/// Ruling B-4, harmful-looking body (`rm -rf ~` as text in a `.rs` file):
/// still Allow, the body is not run by `cargo test`.
#[test]
fn ruling_b4_cargo_test_after_rs_write_harmful_body_allows() {
    let fx = Fixture::new("b4-harmful");
    let cmd = rs("cargo test\n");
    let (d, out) = fx.decision(&cmd);
    assert_eq!(d, "allow", "ruling B-4; command {cmd:?}; got {out:?}");
}

/// Controls for B-4: a written `.sh` holding the harmful body and then run in
/// the same command stays not-Allow. (Observed at origin/main 04d9b8336: the
/// same shapes with the benign body `echo hi` are Allow, because a `.sh` body
/// is judged as shell and `echo hi` is harmless, so benign-body variants are
/// not pinned here.)
fn sh_then(run: &str) -> String {
    format!("cat > x.sh <<'EOF'\nrm -rf ~\nEOF\n{run}\n")
}

hostile!(ruling_b4_control_sh_run, sh_then("sh x.sh"));
hostile!(ruling_b4_control_bash_run, sh_then("bash x.sh"));
hostile!(ruling_b4_control_exec_run, sh_then("./x.sh"));
hostile!(ruling_b4_control_dot_source, sh_then(". ./x.sh"));
hostile!(later_quoted_sh_head, rs("\"sh\" {DIR}/src/x.rs\n"));
hostile!(later_single_quoted_sh_head, rs("'sh' {DIR}/src/x.rs\n"));
hostile!(later_cat_pipe_sh, rs("cat {DIR}/src/x.rs | sh\n"));
hostile!(later_cat_pipe_sh_nospace, rs("cat {DIR}/src/x.rs|sh\n"));
hostile!(later_ls_semicolon_sh, rs("ls; sh {DIR}/src/x.rs\n"));
hostile!(later_ls_semicolon_sh_nospace, rs("ls;sh {DIR}/src/x.rs\n"));
hostile!(later_ls_and_sh, rs("ls && sh {DIR}/src/x.rs\n"));
hostile!(later_true_and_sh_nospace, rs("true&&sh {DIR}/src/x.rs\n"));
hostile!(later_ls_or_sh, rs("ls || sh {DIR}/src/x.rs\n"));
hostile!(later_ls_background_sh, rs("ls & sh {DIR}/src/x.rs\n"));
hostile!(later_ls_background_sh_nospace, rs("ls&sh {DIR}/src/x.rs\n"));
hostile!(
    later_second_line_sh,
    rs("ls {DIR}/src\nsh {DIR}/src/x.rs\n")
);
hostile!(later_comment_then_sh, rs("# ls\nsh {DIR}/src/x.rs\n"));
hostile!(later_tab_indented_sh, rs("\tsh {DIR}/src/x.rs\n"));
hostile!(later_unknown_word, rs("frobnicate {DIR}/src/x.rs\n"));
hostile!(later_assignment_head, rs("PATH={DIR}/src ls\n"));
hostile!(later_path_qualified_ls, rs("/bin/ls {DIR}\n"));
hostile!(
    later_second_heredoc_to_sh_file_then_run,
    rs("cat > {DIR}/src/y.sh <<'B'\nrm -rf ~\nB\nsh {DIR}/src/y.sh\n")
);
hostile!(
    later_line_continuation,
    rs("ls \\\n && sh {DIR}/src/x.rs\n")
);
hostile!(later_subshell_in_arg, rs("ls $(sh {DIR}/src/x.rs)\n"));
hostile!(later_backtick_in_arg, rs("ls `sh {DIR}/src/x.rs`\n"));
hostile!(later_redirect_input_sh, rs("sh < {DIR}/src/x.rs\n"));
hostile!(
    later_echo_redirect_clobbers,
    rs("echo hi > {HOME}/.zshrc\n")
);
// Re-anchored to user ruling 2026-11-18 (heredoc body by target language):
// the old command `rs("echo 'a\nsh {DIR}/src/x.rs\n'\n")` has `sh …` only
// inside echo's quoted argument, so nothing runs the written `.rs` and the
// ruling makes it Allow (`later_multi_line_quote_hides_sh_ruled_allow`
// below). The hostile case now pins a multi-line quote FOLLOWED by a real
// `sh` line: the quote must not hide the command after it.
hostile!(
    later_multi_line_quote_hides_sh,
    rs("echo 'a\nb\n'\nsh {DIR}/src/x.rs\n")
);

/// Ruling 2026-11-18 side of `later_multi_line_quote_hides_sh`'s old command:
/// `sh {DIR}/src/x.rs` is text inside echo's quoted argument, nothing runs the
/// written `.rs`, so its body is data and the command is Allow.
#[test]
fn later_multi_line_quote_hides_sh_ruled_allow() {
    let fx = Fixture::new("mlq-ruled-allow");
    let cmd = rs("echo 'a\nsh {DIR}/src/x.rs\n'\n");
    let (d, out) = fx.decision(&cmd);
    assert_eq!(
        d, "allow",
        "ruling 2026-11-18; command {cmd:?}; got {out:?}"
    );
}

// ---- inert-list tools that can execute a command ---------------------------

hostile!(later_rg_pre_sh, rs("rg --pre sh . {DIR}/src\n"));
hostile!(later_rg_pre_eq_sh, rs("rg --pre=sh . {DIR}/src\n"));
hostile!(
    later_rg_hostname_bin,
    rs("rg --hostname-bin sh -n . {DIR}/src\n")
);
hostile!(later_cat_dev_tcp, rs("cat /dev/tcp/127.0.0.1/1\n"));

// ---- output targets ----------------------------------------------------------

fn opener(consumer: &str, tail: &str) -> String {
    format!("{consumer} <<'EOF'\nrm -rf ~\nEOF\n{tail}")
}

hostile!(
    tee_second_target_sh,
    opener("tee {DIR}/src/x.rs {DIR}/src/y.sh", "ls\n")
);
hostile!(
    tee_second_target_no_ext,
    opener("tee {DIR}/src/x.rs {DIR}/src/run", "ls\n")
);
hostile!(
    tee_second_target_bashrc,
    opener("tee {DIR}/src/x.rs {HOME}/.bashrc", "ls\n")
);
hostile!(
    tee_second_target_tilde_zshrc,
    opener("tee {DIR}/src/x.rs ~/.zshrc", "ls\n")
);
hostile!(
    tee_a_flag_then_zshrc,
    opener("tee -a {DIR}/src/x.rs {HOME}/.zshrc", "ls\n")
);
hostile!(
    tee_stdout_piped_to_sh,
    opener("tee {DIR}/src/x.rs | sh", "ls\n")
);
hostile!(
    tee_dev_stdout_piped_to_sh,
    opener("tee {DIR}/src/x.rs /dev/stdout | sh", "ls\n")
);
hostile!(
    tee_unknown_flag_as_file,
    opener("tee -i {DIR}/src/x.rs", "ls\n")
);
hostile!(
    tee_flag_p,
    opener("tee -p {DIR}/src/x.rs", "sh {DIR}/src/x.rs\n")
);
hostile!(
    cat_two_redirects_sh,
    opener("cat > {DIR}/src/x.rs > {DIR}/src/y.sh", "ls\n")
);
hostile!(
    cat_two_redirects_dotfile,
    opener("cat > {DIR}/src/x.rs > {HOME}/.bashrc", "ls\n")
);
hostile!(
    cat_redirect_append_dotfile,
    opener("cat > {DIR}/src/x.rs >> {HOME}/.zshrc", "ls\n")
);
hostile!(
    cat_then_run_same_line,
    opener("cat > {DIR}/src/x.rs", "ls\n").replacen("<<'EOF'", "<<'EOF' && sh {DIR}/src/x.rs", 1)
);
hostile!(
    cat_semicolon_run_same_line,
    opener("cat > {DIR}/src/x.rs", "ls\n").replacen("<<'EOF'", "<<'EOF'; sh {DIR}/src/x.rs", 1)
);
hostile!(
    cat_pipe_sh_same_line,
    opener("cat > {DIR}/src/x.rs", "ls\n").replacen("<<'EOF'", "<<'EOF' | sh", 1)
);
hostile!(
    cat_pipe_only_to_sh,
    opener("cat", "ls\n").replacen("<<'EOF'", "<<'EOF' | sh", 1)
);
// Re-anchored to user ruling 2026-11-18: the old tail was `ls\n`, so nobody
// runs the written `.rs` and the ruling makes it Allow
// (`cat_dash_then_stdout_sh_ruled_allow` below). The hostile case now runs
// the written file with `sh`, which the ruling keeps restricted.
hostile!(
    cat_dash_then_stdout_sh,
    opener("cat - > {DIR}/src/x.rs 2>&1", "sh {DIR}/src/x.rs\n")
);

/// Ruling 2026-11-18 side of `cat_dash_then_stdout_sh`'s old command: the
/// `.rs` body written by `cat - > … 2>&1` is never run (`ls` only), so it is
/// data and the command is Allow.
#[test]
fn cat_dash_then_stdout_sh_ruled_allow() {
    let fx = Fixture::new("catdash-ruled-allow");
    let cmd = opener("cat - > {DIR}/src/x.rs 2>&1", "ls\n");
    let (d, out) = fx.decision(&cmd);
    assert_eq!(
        d, "allow",
        "ruling 2026-11-18; command {cmd:?}; got {out:?}"
    );
}
hostile!(
    cat_no_target_runs_nothing_but_pipe,
    opener("cat {DIR}/src/x.rs", "sh\n")
);
hostile!(target_glob, opener("cat > {DIR}/src/*.rs", "ls\n"));
// Re-anchored to user ruling 2026-11-18 (and SUBSET.md ruling 12: `$HOME` is
// a Known value): the old tail was `ls\n`, so the `.rs` body landing in
// `$HOME/x.rs` is never run and the ruling makes it Allow
// (`target_var_ruled_allow` below). The hostile case now runs that file with
// `sh` through the same `$HOME` spelling.
hostile!(target_var, opener("cat > $HOME/x.rs", "sh $HOME/x.rs\n"));

/// Ruling 2026-11-18 side of `target_var`'s old command: a `.rs` body written
/// to `$HOME/x.rs` (Known `HOME`, ruling 12) and then only `ls` is data, Allow.
#[test]
fn target_var_ruled_allow() {
    let fx = Fixture::new("targetvar-ruled-allow");
    let cmd = opener("cat > $HOME/x.rs", "ls\n");
    let (d, out) = fx.decision(&cmd);
    assert_eq!(
        d, "allow",
        "ruling 2026-11-18; command {cmd:?}; got {out:?}"
    );
}
hostile!(
    target_cmdsub,
    opener("cat > $(echo {HOME}/.zshrc).rs", "ls\n")
);
hostile!(
    target_dotted_dir_dotfile,
    opener("cat > {HOME}/.zshrc/..rs", "ls\n")
);
hostile!(
    target_upper_ext_rs_then_run,
    opener("cat > {DIR}/src/X.RS", "sh {DIR}/src/X.RS\n")
);
hostile!(
    target_hidden_rs_name,
    opener("cat > {DIR}/src/.rs", "sh {DIR}/src/.rs\n")
);
hostile!(target_trailing_dot, opener("cat > {DIR}/src/x.rs.", "ls\n"));
hostile!(
    target_double_ext_sh_rs,
    opener("cat > {DIR}/src/x.sh.rs", "sh {DIR}/src/x.sh.rs\n")
);
hostile!(target_outside_repo_etc, opener("cat > /etc/x.rs", "ls\n"));
hostile!(
    target_launchagents,
    opener("cat > {HOME}/Library/LaunchAgents/x.js", "ls\n")
);

// ---- delimiters, openers, shape ----------------------------------------------

hostile!(
    unquoted_delimiter_command_substitution,
    "cat > {DIR}/src/x.rs <<EOF\n$(rm -rf ~)\nEOF\nls\n".to_string()
);
hostile!(
    unquoted_delimiter_backtick,
    "cat > {DIR}/src/x.rs <<EOF\n`rm -rf ~`\nEOF\nls\n".to_string()
);
hostile!(
    delimiter_backslash_inside_closes_early,
    "cat > {DIR}/src/x.rs <<E\\OF\nx\nEOF\nrm -rf ~\nE\\OF\n".to_string()
);
hostile!(
    delimiter_quote_inside_closes_early,
    "cat > {DIR}/src/x.rs <<'EO'F\nx\nEOF\nrm -rf ~\nEO'F\n".to_string()
);
hostile!(
    delimiter_double_quote_inside,
    "cat > {DIR}/src/x.rs <<\"EO\"F\nx\nEOF\nrm -rf ~\nEOF\nEOF\n".to_string()
);
hostile!(
    delimiter_empty_quotes,
    "cat > {DIR}/src/x.rs <<''\nx\n\nrm -rf ~\n\n".to_string()
);
hostile!(
    delimiter_glued_tail_concat,
    "cat > {DIR}/src/x.rs <<'EOF'EOF\nx\nEOF\nrm -rf ~\nEOFEOF\n".to_string()
);
hostile!(
    delimiter_dollar_quote,
    "cat > {DIR}/src/x.rs <<$'EOF'\nx\nEOF\nrm -rf ~\n".to_string()
);
hostile!(
    delimiter_glued_pipe,
    "cat > {DIR}/src/x.rs <<'EOF'|sh\nrm -rf ~\nEOF\nls\n".to_string()
);
hostile!(
    delimiter_early_exact_line_then_rm,
    "cat > {DIR}/src/x.rs <<'EOF'\nx\nEOF\nrm -rf ~\nEOF\n".to_string()
);
hostile!(
    delimiter_dash_tab_close_then_sh,
    "cat > {DIR}/src/x.rs <<-'EOF'\nrm -rf ~\n\tEOF\nsh {DIR}/src/x.rs\n".to_string()
);
hostile!(
    two_heredocs_one_opener_line,
    "cat > {DIR}/src/x.rs <<'A' > {DIR}/src/y.sh <<'B'\nrm -rf ~\nA\nrm -rf ~\nB\nls\n".to_string()
);
hostile!(
    unclosed_body_then_rm,
    "cat > {DIR}/src/x.rs <<'EOF'\nrm -rf ~\n".to_string()
);
hostile!(
    prefix_mkdir_then_write_launchagent,
    "mkdir -p {HOME}/Library/LaunchAgents\ncat > {HOME}/Library/LaunchAgents/x.js <<'EOF'\nrm -rf ~\nEOF\nls\n".to_string()
);

// ---- CRLF --------------------------------------------------------------------

hostile!(
    crlf_whole_command_then_sh,
    "cat > {DIR}/src/x.rs <<'EOF'\r\nrm -rf ~\r\nEOF\r\nsh {DIR}/src/x.rs\r\n".to_string()
);
hostile!(
    crlf_close_only_then_sh,
    "cat > {DIR}/src/x.rs <<'EOF'\nrm -rf ~\nEOF\r\nsh {DIR}/src/x.rs\n".to_string()
);
hostile!(
    crlf_body_lines_then_rm_after_fake_close,
    "cat > {DIR}/src/x.rs <<'EOF'\r\nx\r\nEOF\nrm -rf ~\nEOF\r\n".to_string()
);
hostile!(
    crlf_tail_line_ls_cr_then_sh,
    rs("ls\r\nsh {DIR}/src/x.rs\r\n")
);
hostile!(bare_cr_between_tail_commands, rs("ls\rsh {DIR}/src/x.rs\n"));

// ---- shebangs ----------------------------------------------------------------

hostile!(
    shebang_first_line_then_run,
    "cat > {DIR}/src/x.rs <<'EOF'\n#!/bin/sh\nrm -rf ~\nEOF\nls\n".to_string()
);
hostile!(
    shebang_with_space_then_chmod_run,
    "cat > {DIR}/src/x.rs <<'EOF'\n#! /bin/sh\nrm -rf ~\nEOF\nchmod +x {DIR}/src/x.rs\n"
        .to_string()
);
hostile!(
    shebang_crlf_first_line,
    "cat > {DIR}/src/x.rs <<'EOF'\r\n#!/bin/sh\r\nrm -rf ~\r\nEOF\r\nls\r\n".to_string()
);
hostile!(
    inner_attribute_lookalike_shebang_then_run,
    "cat > {DIR}/src/x.rs <<'EOF'\n#![/bin/sh\nrm -rf ~\nEOF\n{DIR}/src/x.rs\n".to_string()
);
hostile!(
    js_hashbang_then_run,
    "cat > {DIR}/src/x.js <<'EOF'\n#!/usr/bin/env node\nrequire('child_process').execSync('rm -rf ~')\nEOF\n{DIR}/src/x.js\n".to_string()
);

// ---- unicode ------------------------------------------------------------------

hostile!(
    unicode_cyrillic_extension_rs,
    "cat > {DIR}/src/x.r\u{0455} <<'EOF'\nrm -rf ~\nEOF\nls\n".to_string()
);
hostile!(
    unicode_fullwidth_dot_extension,
    "cat > {DIR}/src/x\u{ff0e}rs <<'EOF'\nrm -rf ~\nEOF\nsh {DIR}/src/x\u{ff0e}rs\n".to_string()
);
hostile!(
    unicode_nbsp_before_sh_after_close,
    rs("\u{a0}sh {DIR}/src/x.rs\n")
);
hostile!(
    unicode_ideographic_space_between_commands,
    rs("ls\u{3000}; sh {DIR}/src/x.rs\n")
);
hostile!(
    unicode_zero_width_in_head,
    rs("s\u{200b}h {DIR}/src/x.rs\n")
);
hostile!(
    unicode_fullwidth_semicolon_then_sh,
    rs("ls\u{ff1b}sh {DIR}/src/x.rs\n")
);
hostile!(
    unicode_lookalike_delimiter_close,
    "cat > {DIR}/src/x.rs <<'\u{0415}OF'\nx\nEOF\nrm -rf ~\n\u{0415}OF\n".to_string()
);
hostile!(
    unicode_ls_lookalike_head_runs_nothing_but_not_inert,
    rs("l\u{0455} {DIR}\n")
);

// ---- symlinked targets ---------------------------------------------------------

fn to(target: &str) -> String {
    format!("cat > {{DIR}}/{target} <<'EOF'\nrm -rf ~\nEOF\nls\n")
}

hostile!(symlink_rs_to_home_zshrc_cat, to("link.rs"));
hostile!(
    symlink_rs_to_home_zshrc_tee,
    "tee {DIR}/link.rs <<'EOF'\nrm -rf ~\nEOF\nls\n".to_string()
);
hostile!(
    symlink_rs_to_home_zshrc_tee_append,
    "tee -a {DIR}/link.rs <<'EOF'\nrm -rf ~\nEOF\nls\n".to_string()
);
hostile!(symlink_rs_to_git_pre_commit_hook, to("hook.rs"));
hostile!(symlink_rs_to_etc_hosts, to("etc.rs"));
hostile!(symlink_rs_dangling_to_home_bashrc, to("dang.rs"));
hostile!(symlink_js_to_home_zshrc, to("link.js"));
hostile!(
    symlink_rs_relative_cwd_spelling,
    "cat > link.rs <<'EOF'\nrm -rf ~\nEOF\nls\n".to_string()
);
hostile!(
    symlink_rs_via_dot_slash,
    "cat > ./link.rs <<'EOF'\nrm -rf ~\nEOF\nls\n".to_string()
);
hostile!(
    symlink_rs_second_tee_target,
    "tee {DIR}/src/x.rs {DIR}/link.rs <<'EOF'\nrm -rf ~\nEOF\nls\n".to_string()
);
