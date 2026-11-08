// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Contract C2: the `fde gate` resolves the path words of a Bash command.
//!
//! Written by an independent test author (CLAUDE.md §2(a)); the implementer
//! must match the contract, not edit these tests to pass.
//!
//! Before C2 the gate denied a Bash call only when the payload `cwd` lay in a
//! guarded area or when the command TEXT contained the root / worktree path.
//! (Since the user ruling 2026-11-08 — gates judge own worktree only — a
//! guarded target's area is its registered root alone; a linked worktree of the
//! target is no longer part of it, see
//! `relative_word_reaching_linked_worktree_is_not_denied`.)
//! Every DENY case below deliberately avoids both: the payload cwd is the
//! parent `P` of the target `T` (`P/proj`), a sibling dir, or an unrelated temp
//! dir, and the target is only reached through a relative word, a symlink
//! (`P/link -> T`), `~` with `HOME=T`, an `--opt=value` / `NAME=value` value,
//! or not resolvable at all (`$`, backtick, glob, brace, `~user`, unterminated
//! quote). Each test first checks that its cwd and command meet those
//! conditions, so a deny can only come from the new rule.
//!
//! Anti-vacuity: every DENY list is replayed against the same layout with the
//! target ratified, and with nothing registered — those must all ALLOW, so an
//! always-deny gate fails. ALLOW controls with a guarded target (single-quoted
//! glob, plain text, `~/` with an unrelated HOME, unrelated commands, a
//! sibling dir whose name merely starts with the target's) fail a gate that
//! denies every Bash call while something is guarded.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::{json, Value};

use common::{code, init_repo_at, pretooluse_payload, run_fde, show, stderr, TempPlace};

// ------------------------------------------------------------------ fixture

/// How the target is registered in a [`Layout`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Reg {
    /// `fde begin` only: guarded.
    Unratified,
    /// `fde begin` + `fde ratify`: not guarded.
    Ratified,
    /// Nothing registered at all.
    None,
}

/// `P/` (parent) holding the target `P/proj`, a sibling dir `P/sib`, a plain
/// file `P/a.txt`, a symlink `P/link -> P/proj`, and a dir `P/proj2` whose
/// name starts with the target's. State and spec live in separate temp dirs.
struct Layout {
    parent: TempPlace,
    root: PathBuf,
    sib: PathBuf,
    unrelated: TempPlace,
    state: TempPlace,
    spec_dir: TempPlace,
}

impl Layout {
    fn new(reg: Reg) -> Self {
        let parent = TempPlace::new("fde-c2-parent-");
        let root = init_repo_at(&parent.path.join("proj"));
        let sib = parent.path.join("sib");
        fs::create_dir_all(&sib).expect("create sib");
        fs::create_dir_all(parent.path.join("proj2")).expect("create proj2");
        fs::write(parent.path.join("a.txt"), "a\n").expect("write a.txt");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&root, parent.path.join("link")).expect("symlink link -> T");
        let unrelated = TempPlace::new("fde-c2-unrelated-");
        fs::write(unrelated.path.join("notes.txt"), "n\n").expect("write notes.txt");
        let state = TempPlace::new("fde-c2-state-");
        let spec_dir = TempPlace::new("fde-c2-spec-");
        let spec = common::write_spec(&spec_dir.path, "spec.toml", "c2", &[("R1", "one")]);
        let spec = spec.to_str().expect("utf-8 spec").to_string();
        let l = Layout {
            parent,
            root,
            sib,
            unrelated,
            state,
            spec_dir,
        };
        let expect_status = match reg {
            Reg::None => None,
            Reg::Unratified => {
                l.fde_ok(&["begin", "--spec", &spec]);
                Some(1)
            }
            Reg::Ratified => {
                l.fde_ok(&["begin", "--spec", &spec]);
                l.fde_ok(&["ratify", "--spec", &spec]);
                Some(0)
            }
        };
        if let Some(want) = expect_status {
            let st = run_fde(&l.root, &l.state.path, &["status"]);
            assert_eq!(
                code(&st),
                Some(want),
                "fixture: status of a {reg:?} target: {}",
                show(&st)
            );
        }
        l
    }

    fn fde_ok(&self, args: &[&str]) {
        let out = run_fde(&self.root, &self.state.path, args);
        assert_eq!(code(&out), Some(0), "fixture: fde {args:?}: {}", show(&out));
    }

    fn p(&self) -> &Path {
        &self.parent.path
    }

    /// Run the gate on Bash `cmd` with payload cwd `cwd`; the gate's process
    /// cwd is the neutral spec dir. `env` entries: `Some(v)` sets, `None`
    /// removes (applied after the git leak vars are removed).
    fn gate_env(&self, cwd: &Path, cmd: &str, env: &[(&str, Option<&Path>)]) -> Output {
        use std::io::Write;
        let payload = pretooluse_payload(
            cwd,
            "Bash",
            json!({"command": cmd, "description": "c2 fixture"}),
        );
        let mut c = Command::new(common::fde_bin());
        c.current_dir(&self.spec_dir.path)
            .arg("gate")
            .env("FDE_STATE_DIR", &self.state.path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for v in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"] {
            c.env_remove(v);
        }
        for (k, v) in env {
            match v {
                Some(v) => {
                    c.env(k, v);
                }
                None => {
                    c.env_remove(k);
                }
            }
        }
        let mut child = c.spawn().expect("spawn fde gate");
        {
            let mut si = child.stdin.take().expect("gate stdin");
            let _ = si.write_all(payload.as_bytes());
        }
        let out = child.wait_with_output().expect("wait fde gate");
        if code(&out) == Some(0) {
            assert_eq!(
                decision(&out).as_deref(),
                Some("allow"),
                "exit 0 must carry permissionDecision \"allow\": {}",
                show(&out)
            );
        }
        out
    }

    fn gate(&self, cwd: &Path, cmd: &str) -> Output {
        self.gate_env(cwd, cmd, &[])
    }
}

fn decision(out: &Output) -> Option<String> {
    let v: Value = serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).ok()?;
    v.get("hookSpecificOutput")?
        .get("permissionDecision")?
        .as_str()
        .map(str::to_string)
}

fn reason(out: &Output) -> String {
    let v: Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).unwrap_or(Value::Null);
    v.get("hookSpecificOutput")
        .and_then(|h| h.get("permissionDecisionReason"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn is_denied(out: &Output) -> bool {
    decision(out).as_deref() == Some("deny") && code(out) == Some(2)
}

fn is_allowed(out: &Output) -> bool {
    decision(out).as_deref() == Some("allow") && code(out) == Some(0)
}

/// A case is `(cwd, command)`.
type Case = (PathBuf, String);

/// Pre-conditions every DENY case must satisfy so that only the C2 rule can
/// deny it: the payload cwd is outside the target, and the command text does
/// not contain the target root path.
fn assert_old_rules_cannot_see(l: &Layout, cases: &[Case]) {
    let root = l.root.to_str().expect("utf-8 root");
    for (cwd, cmd) in cases {
        let ccwd = cwd.canonicalize().expect("canonicalize case cwd");
        assert!(
            !ccwd.starts_with(&l.root),
            "fixture: case {cmd:?} must run outside the target, cwd {}",
            cwd.display()
        );
        assert!(
            !cmd.contains(root),
            "fixture: case {cmd:?} must not spell the target root path"
        );
    }
}

/// Run every case; collect the ones whose answer is not `want`, with output.
fn check_all(l: &Layout, cases: &[Case], want_deny: bool, label: &str) {
    let mut bad = Vec::new();
    for (cwd, cmd) in cases {
        let out = l.gate(cwd, cmd);
        let ok = if want_deny {
            is_denied(&out)
        } else {
            is_allowed(&out)
        };
        if !ok {
            bad.push(format!(
                "  [{label}] Bash {cmd:?} (cwd {}) must be {}:\n{}",
                cwd.display(),
                if want_deny { "DENIED" } else { "ALLOWED" },
                show(&out)
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "{} of {} case(s) wrong:\n{}",
        bad.len(),
        cases.len(),
        bad.join("\n")
    );
}

/// The DENY list against guarded target T, built for layout `l` (same
/// relative spellings in every layout).
fn relative_cases(l: &Layout) -> Vec<Case> {
    let p = l.p().to_path_buf();
    vec![
        (l.sib.clone(), "touch ../proj/x".into()),
        (p.clone(), "rm -rf proj".into()),
        (p.clone(), "cp a.txt proj/".into()),
        (p.clone(), "ls ./proj".into()),
        (p.clone(), "git -C proj status".into()),
        (p.clone(), "cat proj/README.md".into()),
        (p.clone(), "true; touch proj/x".into()),
        (p.clone(), "echo hi>proj/x".into()),
        (p.clone(), "echo hi 2>proj/x".into()),
        (p.clone(), "(cd /tmp)|tee proj/x".into()),
        (p.clone(), "echo hi > \"proj/x\"".into()),
        (p.clone(), "echo hi > 'proj/x'".into()),
        (p.clone(), "touch pr\\oj/x".into()),
        (p, "touch sib/../proj/x".into()),
    ]
}

fn symlink_cases(l: &Layout) -> Vec<Case> {
    let p = l.p().to_path_buf();
    vec![
        (p.clone(), "echo hi > link/x".into()),
        (p.clone(), "cat link/README.md".into()),
        (p.clone(), "cat <link/README.md".into()),
        (l.sib.clone(), "touch ../link/new-file".into()),
    ]
}

fn option_value_cases(l: &Layout) -> Vec<Case> {
    let p = l.p().to_path_buf();
    vec![
        (
            p.clone(),
            "curl --output=proj/x https://example.invalid".into(),
        ),
        (p.clone(), "somecmd --out=link/x".into()),
        (p, "FOO=proj/x true".into()),
    ]
}

/// Commands with a word the gate cannot resolve, run from an unrelated dir.
fn unresolvable_cases(l: &Layout) -> Vec<Case> {
    let u = l.unrelated.path.clone();
    [
        "echo $HOME",
        "echo \"$HOME\"",
        "echo ${HOME}",
        "echo `pwd`",
        "echo \"`pwd`\"",
        "ls *",
        "ls ?",
        "ls [ab]",
        "echo {a,b}",
        "ls ~root",
        "echo 'abc",
        "echo \"abc",
    ]
    .iter()
    .map(|c| (u.clone(), (*c).to_string()))
    .collect()
}

/// A path word whose canonicalization fails with something other than
/// NotFound (a regular file used as a directory).
fn canonicalize_error_cases(l: &Layout) -> Vec<Case> {
    vec![(l.unrelated.path.clone(), "cat notes.txt/x".into())]
}

// ------------------------------------------------------------- DENY (RED)

/// C2 rule 3: relative words resolved against the payload cwd that land in
/// the guarded target are denied (redirect targets, quoted words, backslash
/// escapes and `..` included).
#[test]
fn relative_words_reaching_unratified_target_are_denied() {
    let l = Layout::new(Reg::Unratified);
    let cases = relative_cases(&l);
    assert_old_rules_cannot_see(&l, &cases);
    check_all(&l, &cases, true, "relative word");
}

/// C2 rule 3: `canonical_lenient` follows symlinks, so a word through
/// `P/link -> T` lands in the target.
#[cfg(unix)]
#[test]
fn symlinked_words_reaching_unratified_target_are_denied() {
    let l = Layout::new(Reg::Unratified);
    let cases = symlink_cases(&l);
    assert_old_rules_cannot_see(&l, &cases);
    check_all(&l, &cases, true, "symlink word");
}

/// C2 rule 3: the value of `--opt=value` and of a `NAME=value` prefix is
/// checked like a word.
#[test]
fn option_and_assignment_values_reaching_unratified_target_are_denied() {
    let l = Layout::new(Reg::Unratified);
    let cases = option_value_cases(&l);
    assert_old_rules_cannot_see(&l, &cases);
    check_all(&l, &cases, true, "--opt=/NAME= value");
}

/// INVERTED (user ruling 2026-11-08: gates judge own worktree only — "donegate
/// そのものが他のworktreeを見たり検証したりテストしたりすることは禁止とする。
/// 他のgateでも禁止とする"). Was `relative_word_reaching_linked_worktree_is_denied`
/// (C2 rule 3 "over the whole guarded area": a guarded target's area included
/// every `git worktree list` entry). A guarded target's area is now its
/// registered root ONLY, so a relative word reaching a linked worktree of the
/// target (created outside the target dir) is NOT denied. Anti-vacuity, same
/// fixture: a relative word reaching the root itself is still denied while
/// unratified (and allowed after ratify).
#[test]
fn relative_word_reaching_linked_worktree_is_not_denied() {
    let l = Layout::new(Reg::Unratified);
    let wt = l.p().join("wt");
    common::git(
        &l.root,
        &["worktree", "add", "-q", "-b", "c2-wt", wt.to_str().unwrap()],
    );
    let wt = wt.canonicalize().unwrap();
    let cases: Vec<Case> = vec![(l.p().to_path_buf(), "touch wt/x".into())];
    assert_old_rules_cannot_see(&l, &cases);
    assert!(!cases[0].1.contains(wt.to_str().unwrap()));
    check_all(&l, &cases, false, "worktree word, unratified");
    // Also by absolute text and by cwd inside the worktree.
    let abs: Vec<Case> = vec![
        (l.p().to_path_buf(), format!("touch {}/x", wt.display())),
        (wt.clone(), "touch y".into()),
    ];
    check_all(&l, &abs, false, "worktree path/cwd, unratified");
    // (c) the root itself is still judged in the same fixture.
    let root_case: Vec<Case> = vec![(l.p().to_path_buf(), "touch proj/x".into())];
    assert_old_rules_cannot_see(&l, &root_case);
    check_all(&l, &root_case, true, "root word, unratified");
    // Partner: after ratification the root call is allowed too.
    let spec = l.spec_dir.path.join("spec.toml");
    l.fde_ok(&["ratify", "--spec", spec.to_str().unwrap()]);
    check_all(&l, &root_case, false, "root word, ratified");
}

/// C2 rules 2 and 4: an unresolvable word (`$`, backtick, unquoted glob,
/// brace, `~user`) or an unterminated quote is Undetermined (deny) while a
/// target is guarded — even from a cwd unrelated to it.
#[test]
fn unresolvable_words_with_a_guarded_target_are_denied() {
    let l = Layout::new(Reg::Unratified);
    let cases = unresolvable_cases(&l);
    assert_old_rules_cannot_see(&l, &cases);
    check_all(&l, &cases, true, "unresolvable word");
}

/// C2 rule 4: the deny reason names the unresolvable word and says the gate
/// cannot resolve it.
#[test]
fn unresolvable_word_deny_reason_names_the_word() {
    let l = Layout::new(Reg::Unratified);
    let out = l.gate(&l.unrelated.path, "echo $FDE_C2_MARK");
    assert!(is_denied(&out), "must be denied: {}", show(&out));
    let r = reason(&out);
    assert!(
        r.contains("$FDE_C2_MARK"),
        "deny reason must name the word `$FDE_C2_MARK`: {}",
        show(&out)
    );
    assert!(
        r.to_lowercase().contains("resolve"),
        "deny reason must say the gate cannot resolve the word: {}",
        show(&out)
    );
    assert!(
        stderr(&out).contains("$FDE_C2_MARK"),
        "stderr must carry the same reason: {}",
        show(&out)
    );
}

/// C2 rule 3: a canonicalization error other than NotFound for a word is
/// Undetermined (deny) while a target is guarded.
#[test]
fn word_with_a_canonicalization_error_is_denied() {
    let l = Layout::new(Reg::Unratified);
    let cases = canonicalize_error_cases(&l);
    assert_old_rules_cannot_see(&l, &cases);
    check_all(&l, &cases, true, "canonicalize error");
}

/// C2 rule 2: `~` and `~/...` expand to `$HOME`. With HOME = the target, the
/// words land in it (deny); with HOME unset or empty, `~/` is unresolvable
/// (deny); with an unrelated HOME it is allowed.
#[test]
fn tilde_words_resolve_through_home() {
    let l = Layout::new(Reg::Unratified);
    let u = l.unrelated.path.clone();
    let other_home = TempPlace::new("fde-c2-home-");
    let empty = PathBuf::new();
    let mut bad = Vec::new();
    let mut expect = |cmd: &str, env: &[(&str, Option<&Path>)], deny: bool, what: &str| {
        assert!(!cmd.contains(l.root.to_str().unwrap()));
        let out = l.gate_env(&u, cmd, env);
        let ok = if deny {
            is_denied(&out)
        } else {
            is_allowed(&out)
        };
        if !ok {
            bad.push(format!(
                "  Bash {cmd:?} with {what} must be {}:\n{}",
                if deny { "DENIED" } else { "ALLOWED" },
                show(&out)
            ));
        }
    };
    let home_t = [("HOME", Some(l.root.as_path()))];
    expect("ls ~/", &home_t, true, "HOME = target");
    expect("ls ~", &home_t, true, "HOME = target");
    expect("touch ~/x", &home_t, true, "HOME = target");
    expect("ls ~/", &[("HOME", None)], true, "HOME unset");
    expect("ls ~/", &[("HOME", Some(&empty))], true, "HOME empty");
    let home_o = [("HOME", Some(other_home.path.as_path()))];
    expect("ls ~/", &home_o, false, "HOME unrelated");
    expect("touch ~/x", &home_o, false, "HOME unrelated");
    assert!(
        bad.is_empty(),
        "{} case(s) wrong:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

// ------------------------------------------------------- ALLOW partners

fn every_deny_case(l: &Layout) -> Vec<Case> {
    let mut v = relative_cases(l);
    #[cfg(unix)]
    v.extend(symlink_cases(l));
    v.extend(option_value_cases(l));
    v.extend(unresolvable_cases(l));
    v.extend(canonicalize_error_cases(l));
    v
}

/// C2 rule 4 / anti-vacuity: the very same commands are allowed when the
/// target is ratified (nothing guarded).
#[test]
fn same_commands_are_allowed_when_target_is_ratified() {
    let l = Layout::new(Reg::Ratified);
    check_all(&l, &every_deny_case(&l), false, "ratified");
}

/// C2 rule 4 / anti-vacuity: the very same commands are allowed when nothing
/// is registered.
#[test]
fn same_commands_are_allowed_when_nothing_is_registered() {
    let l = Layout::new(Reg::None);
    check_all(&l, &every_deny_case(&l), false, "nothing registered");
}

/// With a guarded target: resolvable words that land elsewhere are allowed
/// (single-quoted glob is literal, plain double-quoted text, unrelated
/// commands, a sibling dir, a dir whose name only starts with the target's).
#[test]
fn resolvable_words_outside_the_guarded_target_are_allowed() {
    let l = Layout::new(Reg::Unratified);
    let u = l.unrelated.path.clone();
    let p = l.p().to_path_buf();
    let cases: Vec<Case> = vec![
        (u.clone(), "ls '*'".into()),
        (u.clone(), "echo \"plain text\"".into()),
        (u.clone(), "cargo test".into()),
        (u.clone(), "ls /tmp".into()),
        (u.clone(), "echo hi > notes.txt".into()),
        (p.clone(), "ls sib".into()),
        (p.clone(), "touch proj2/x".into()),
        (l.sib.clone(), "cat ../a.txt".into()),
    ];
    check_all(&l, &cases, false, "outside the target");
}

/// C2 rule 5: the exact `fde status|report|begin` allowance still applies
/// when the target is reached through a relative word.
#[test]
fn fde_status_naming_target_relatively_is_still_allowed() {
    let l = Layout::new(Reg::Unratified);
    let p = l.p().to_path_buf();
    let cases: Vec<Case> = vec![
        (p.clone(), "fde status proj".into()),
        (p, "fde report ./proj".into()),
    ];
    check_all(&l, &cases, false, "fde allowance");
}
