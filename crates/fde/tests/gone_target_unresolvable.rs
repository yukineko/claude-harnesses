// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Unresolvable Bash words while a registered target is gone.
//!
//! Written by an independent test author; the implementer must not edit these tests.
//!
//! Contract under test (user ruling 2026-11-04): while any registered target
//! is gone (root deleted, observed NotFound), a Bash call (cwd outside the
//! former root, command not naming the root) containing a word the gate
//! cannot resolve (`$VAR`, backticks, unquoted globs, braces, `~user`,
//! unterminated quotes, canonicalization errors) is denied: decision `deny`,
//! exit 2, and the permissionDecisionReason contains the literal
//! `! fde forget`. Fully resolvable unrelated commands stay allowed, and with
//! no target (or only a present ratified target) unresolvable words stay allowed.

mod common;

use std::fs;
use std::path::PathBuf;
use std::process::Output;

use serde_json::{json, Value};

use common::{
    code, init_repo_at, pretooluse_payload, run_fde, run_gate, show, write_spec, TempPlace,
};

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// Target registered, then its root deleted.
    Gone,
    /// Nothing registered.
    None,
    /// Target registered and ratified, root present.
    RatifiedPresent,
}

struct Layout {
    _parent: TempPlace,
    root: PathBuf,
    unrelated: TempPlace,
    state: TempPlace,
    spec_dir: TempPlace,
}

impl Layout {
    fn new(mode: Mode) -> Self {
        let parent = TempPlace::new("fde-gu-parent-");
        let root = init_repo_at(&parent.path.join("proj"));
        let unrelated = TempPlace::new("fde-gu-unrelated-");
        fs::write(unrelated.path.join("notes.txt"), "n\n").expect("write notes.txt");
        let state = TempPlace::new("fde-gu-state-");
        let spec_dir = TempPlace::new("fde-gu-spec-");
        let spec = write_spec(&spec_dir.path, "spec.toml", "gu", &[("R1", "one")]);
        let spec_s = spec.to_str().expect("utf-8 spec");
        if mode != Mode::None {
            let out = run_fde(&root, &state.path, &["begin", "--spec", spec_s]);
            assert_eq!(code(&out), Some(0), "fixture: begin: {}", show(&out));
        }
        if mode == Mode::RatifiedPresent {
            let out = run_fde(&root, &state.path, &["ratify", "--spec", spec_s]);
            assert_eq!(code(&out), Some(0), "fixture: ratify: {}", show(&out));
        }
        if mode == Mode::Gone {
            fs::remove_dir_all(&root).expect("delete the target root");
            let st = run_fde(
                &parent.path,
                &state.path,
                &["status", root.to_str().unwrap()],
            );
            assert!(
                String::from_utf8_lossy(&st.stderr).contains("gone"),
                "fixture: target must be gone: {}",
                show(&st)
            );
        }
        Layout {
            _parent: parent,
            root,
            unrelated,
            state,
            spec_dir,
        }
    }

    fn gate(&self, cmd: &str) -> Output {
        assert!(
            !cmd.contains(self.root.to_str().unwrap()),
            "fixture: {cmd:?} must not spell the root"
        );
        let payload = pretooluse_payload(&self.unrelated.path, "Bash", json!({"command": cmd}));
        run_gate(&self.spec_dir.path, &self.state.path, payload.as_bytes())
    }
}

fn parsed(out: &Output) -> Option<Value> {
    serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).ok()
}

fn decision(out: &Output) -> Option<String> {
    parsed(out)?
        .get("hookSpecificOutput")?
        .get("permissionDecision")?
        .as_str()
        .map(str::to_string)
}

fn deny_reason(out: &Output) -> String {
    parsed(out)
        .and_then(|v| {
            v.get("hookSpecificOutput")?
                .get("permissionDecisionReason")?
                .as_str()
                .map(str::to_string)
        })
        .unwrap_or_default()
}

fn assert_denied_with_forget_hint(out: &Output, what: &str) {
    assert_eq!(
        decision(out).as_deref(),
        Some("deny"),
        "{what}: must be denied: {}",
        show(out)
    );
    assert_eq!(code(out), Some(2), "{what}: deny exits 2: {}", show(out));
    assert!(
        deny_reason(out).contains("! fde forget"),
        "{what}: reason must contain `! fde forget`: {}",
        show(out)
    );
}

fn assert_allowed(out: &Output, what: &str) {
    assert_eq!(
        decision(out).as_deref(),
        Some("allow"),
        "{what}: must be allowed: {}",
        show(out)
    );
    assert_eq!(code(out), Some(0), "{what}: allow exits 0: {}", show(out));
}

#[test]
fn gone_denies_dollar_var() {
    let l = Layout::new(Mode::Gone);
    let out = l.gate("echo $HOME");
    assert_denied_with_forget_hint(&out, "echo $HOME");
}

#[test]
fn gone_denies_quoted_dollar_var() {
    let l = Layout::new(Mode::Gone);
    let out = l.gate("echo \"$HOME\"");
    assert_denied_with_forget_hint(&out, "echo \"$HOME\"");
}

#[test]
fn gone_denies_backtick_substitution() {
    let l = Layout::new(Mode::Gone);
    let out = l.gate("echo `pwd`");
    assert_denied_with_forget_hint(&out, "echo `pwd`");
}

#[test]
fn gone_denies_star_glob() {
    let l = Layout::new(Mode::Gone);
    let out = l.gate("ls *");
    assert_denied_with_forget_hint(&out, "ls *");
}

#[test]
fn gone_denies_question_glob() {
    let l = Layout::new(Mode::Gone);
    let out = l.gate("ls x?");
    assert_denied_with_forget_hint(&out, "ls x?");
}

#[test]
fn gone_denies_bracket_glob() {
    let l = Layout::new(Mode::Gone);
    let out = l.gate("ls [ab]");
    assert_denied_with_forget_hint(&out, "ls [ab]");
}

#[test]
fn gone_denies_brace_expansion() {
    let l = Layout::new(Mode::Gone);
    let out = l.gate("echo {a,b}");
    assert_denied_with_forget_hint(&out, "echo {a,b}");
}

#[test]
fn gone_denies_tilde_user() {
    let l = Layout::new(Mode::Gone);
    let out = l.gate("ls ~root");
    assert_denied_with_forget_hint(&out, "ls ~root");
}

#[test]
fn gone_denies_unterminated_single_quote() {
    let l = Layout::new(Mode::Gone);
    let out = l.gate("echo 'abc");
    assert_denied_with_forget_hint(&out, "echo 'abc");
}

#[test]
fn gone_denies_unterminated_double_quote() {
    let l = Layout::new(Mode::Gone);
    let out = l.gate("echo \"abc");
    assert_denied_with_forget_hint(&out, "echo \"abc");
}

#[test]
fn gone_denies_canonicalization_error_with_forget_hint() {
    let l = Layout::new(Mode::Gone);
    let out = l.gate("cat notes.txt/x");
    assert_denied_with_forget_hint(&out, "cat notes.txt/x");
}

#[test]
fn gone_allows_fully_resolvable_unrelated_commands_control() {
    let l = Layout::new(Mode::Gone);
    assert_allowed(&l.gate("ls /tmp"), "ls /tmp with a gone target");
    assert_allowed(&l.gate("echo hi"), "echo hi with a gone target");
}

#[test]
fn no_target_allows_unresolvable_word_control() {
    let l = Layout::new(Mode::None);
    assert_allowed(&l.gate("echo $HOME"), "echo $HOME with nothing registered");
}

#[test]
fn ratified_present_target_allows_unresolvable_word_control() {
    let l = Layout::new(Mode::RatifiedPresent);
    assert_allowed(
        &l.gate("echo $HOME"),
        "echo $HOME with only a ratified present target",
    );
}
