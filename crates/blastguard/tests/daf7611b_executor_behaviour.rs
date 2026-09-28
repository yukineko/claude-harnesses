// このファイルは丸ごと integration test なので expect/unwrap を許可する。
#![allow(clippy::expect_used)]
//! Behavioural tests for backlog daf7611b (fix 90ee427c): the text that
//! non-shell executors run (git -c / awk / env -S / deno / bun / osascript /
//! node -r / python -W) is judged, not silently Allowed. Written by an agent
//! that did not write the fix (CLAUDE.md 2(a)). Each case is its own test so a
//! regression names the exact input.

use blastguard::detect;
use blastguard::model::Decision;
use serde_json::json;

fn bash(cmd: &str) -> Decision {
    detect::detect("Bash", Some(&json!({ "command": cmd })))
}

fn assert_deny(cmd: &str) {
    let d = bash(cmd);
    assert!(d.is_deny(), "must be Deny, got {d:?} for: {cmd}");
}

fn assert_ask(cmd: &str) {
    let d = bash(cmd);
    assert!(
        d.is_ask() && !d.is_deny(),
        "must be exactly Ask, got {d:?} for: {cmd}"
    );
}

fn assert_allow(cmd: &str) {
    let d = bash(cmd);
    assert!(
        matches!(d, Decision::Allow),
        "must be Allow, got {d:?} for: {cmd}"
    );
}

#[test]
fn deny_git_alias_shell_rm() {
    assert_deny(r#"git -c alias.x='!rm -rf ~/x' x"#);
}

#[test]
fn deny_git_alias_clean_fdx() {
    assert_deny(r#"git -c alias.x='clean -fdx' x"#);
}

#[test]
fn deny_git_core_editor_rm() {
    assert_deny(r#"git -c core.editor="rm -rf ~/x" commit"#);
}

#[test]
fn deny_git_core_sshcommand_rm() {
    assert_deny(r#"git -c core.sshCommand='rm -rf ~/x' fetch"#);
}

#[test]
fn deny_git_credential_helper_rm() {
    assert_deny(r#"git -c credential.helper='!rm -rf ~/x' fetch"#);
}

#[test]
fn deny_gawk_e_system_rm() {
    assert_deny(r#"gawk -e 'BEGIN{system("rm -rf ~/x")}'"#);
}

#[test]
fn deny_sudo_env_dash_s_rm() {
    assert_deny(r#"sudo env -S "rm -rf ~/x""#);
}

#[test]
fn deny_env_split_string_rm() {
    assert_deny(r#"env --split-string="rm -rf ~/x""#);
}

#[test]
fn deny_deno_repl_eval_remove() {
    assert_deny(r#"deno repl --eval "Deno.removeSync('/Users/yuki/x', {recursive: true})""#);
}

#[test]
fn deny_osascript_js_doshellscript_rm() {
    assert_deny(r#"osascript -l JavaScript -e 'app.doShellScript("rm -rf ~/x")'"#);
}

#[test]
fn deny_node_r_then_e_rmsync() {
    assert_deny(
        r#"node -r ts-node/register -e "require('fs').rmSync('/Users/yuki/x',{recursive:true,force:true})""#,
    );
}

#[test]
fn deny_python3_w_then_c_rmtree() {
    assert_deny(r#"python3 -W ignore -c "import shutil; shutil.rmtree('/Users/yuki/x')""#);
}

#[test]
fn ask_git_c_include_path() {
    assert_ask("git -c include.path=/tmp/evil status");
}

#[test]
fn ask_git_config_env_pager() {
    assert_ask("git --config-env core.pager=EVIL log");
}

#[test]
fn ask_awk_system_dynamic() {
    assert_ask(r#"awk '{system("rm " $1)}' f"#);
}

#[test]
fn ask_awk_pipe_to_sh() {
    assert_ask(r#"awk '{print $1 | "sh"}' f"#);
}

#[test]
fn ask_osascript_do_shell_script_dynamic() {
    assert_ask("osascript -e 'do shell script cmd'");
}

#[test]
fn allow_git_c_pager_log_false() {
    assert_allow("git -c pager.log=false log");
}

#[test]
fn allow_git_c_fsmonitor_true() {
    assert_allow("git -c core.fsmonitor=true status");
}

#[test]
fn allow_git_c_pager_less() {
    assert_allow("git -c core.pager='less -R' log");
}

#[test]
fn allow_git_c_user_name() {
    assert_allow("git -c user.name=x commit -m hi");
}

#[test]
fn allow_awk_pipe_sort() {
    assert_allow(r#"awk '{print $1 | "sort"}' f"#);
}

#[test]
fn allow_awk_regex_alternation() {
    assert_allow("awk '/foo|bar/ {print}' f");
}

#[test]
fn allow_awk_fs_pipe() {
    assert_allow("awk -F'|' '{print $2}' f");
}

#[test]
fn allow_awk_date_getline() {
    assert_allow(r#"awk 'BEGIN{while(("date" | getline d) > 0) print d}'"#);
}

#[test]
fn allow_awk_dash_f_progfile() {
    assert_allow("awk -f prog.awk data");
}

#[test]
fn allow_env_dash_s_ls() {
    assert_allow("env -S 'ls -la'");
}

#[test]
fn allow_deno_eval_console() {
    assert_allow(r#"deno eval "console.log(1)""#);
}

#[test]
fn allow_deno_run_main() {
    assert_allow("deno run main.ts");
}

#[test]
fn allow_bun_e_console() {
    assert_allow(r#"bun -e "console.log(1)""#);
}

#[test]
fn allow_bun_run_build() {
    assert_allow("bun run build");
}

#[test]
fn allow_bun_test() {
    assert_allow("bun test");
}

#[test]
fn allow_osascript_scpt() {
    assert_allow("osascript script.scpt");
}
