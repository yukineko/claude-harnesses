#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED(ask-rules): declarative Allow/Deny rules for Asks, through the hook.
//!
//! Spec: docs/specs/blastguard-ask-learning.md, "Rule files and the judging
//! module". Written by an agent that does not implement it (CLAUDE.md 2(a)).
//! Black-box: every test spawns the built binary with a hook JSON on stdin,
//! HOME in a sandbox and an explicit cwd. The human rules file is replaced
//! with env `BLASTGUARD_ASK_RULES=<path>`; the learned table is pinned to a
//! missing path.
//!
//! Shape keys are never hard-coded: they come from `blastguard shape` and the
//! rule files are generated from them.
//!
//! Every NEGATIVE test (an allow rule must be ignored) also asserts a
//! canary: the same file DOES allow a harmless shape. Without it, a negative
//! test passes today on an implementation that reads no rules at all, i.e. it
//! would prove nothing. The pure matcher is covered by
//! ask_rules_unit_20261128.rs.
//!
//! What these do NOT prove: the matcher's finer semantics (flags_any/none,
//! program hash, multi-verb) -- see the unit file; the system and
//! protected-config classes (an Ask never carries them -- the detector Denies
//! those first), also unit-only.

#[path = "support/bounded.rs"]
mod bounded;

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const SID: &str = "5d0c1f0e-aaaa-4bbb-8ccc-000000000002";

struct Env {
    root: PathBuf,
    home: PathBuf,
    main: PathBuf, // main working tree of a temp repo
    wt: PathBuf,   // linked worktree of it
}

fn git(dir: &Path, args: &[&str]) {
    let st = Command::new("git")
        .args(["-c", "user.email=t@t", "-c", "user.name=t"])
        .args(args)
        .current_dir(dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?} failed in {dir:?}");
}

fn env(name: &str) -> Env {
    // NOT std::env::temp_dir(): /var/folders/... is a SYSTEM_PREFIX, so every
    // repo under it classes as `s3-system` (measured by the coordinator).
    let root = PathBuf::from(format!(
        "/private/tmp/claude-askrules/{}-{}",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    // Under the sandbox HOME, NOT the temp dir: /var/folders/... is a
    // SYSTEM_PREFIX, so a repo there classes as `s3-system`.
    let main = home.join("repo");
    // fixture re-anchored 2026-11-28 (leader ruling): fixed container / scratchpad positions match the real layout
    let wt = home.join(".harness-worktrees").join("wt");
    std::fs::create_dir_all(home.join(".harness-worktrees")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&main).unwrap();
    git(&main, &["init", "-q"]);
    git(&main, &["commit", "-q", "--allow-empty", "-m", "i"]);
    git(
        &main,
        &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "w"],
    );
    Env {
        root,
        home,
        main,
        wt,
    }
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

fn spawn(e: &Env, rules: Option<&Path>, args: &[&str], stdin: &str) -> Run {
    let mut c = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    c.args(args)
        .env("HOME", &e.home)
        .env("BLASTGUARD_BLOCK_LOG_DIR", e.root.join("blocklog"))
        .env("BLASTGUARD_RELAY_DIR", e.root.join("relay"))
        .env("BLASTGUARD_APPROVALS_DIR", e.root.join("approvals"))
        .env("BLASTGUARD_LEARNED_TABLE", e.root.join("no-table.json"))
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("BLASTGUARD_ASK")
        .env_remove("CLAUDE_PROJECT_DIR")
        .env_remove("BLASTGUARD_ASK_RULES")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(p) = rules {
        c.env("BLASTGUARD_ASK_RULES", p);
    }
    let out = bounded::run_bounded(&mut c, stdin.as_bytes(), bounded::HOOK_DEADLINE);
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn bash(cwd: &Path, command: &str) -> Value {
    json!({
        "hook_event_name": "PreToolUse",
        "session_id": SID,
        "cwd": cwd.to_str().unwrap(),
        "tool_name": "Bash",
        "tool_input": { "command": command },
    })
}

fn decision(r: &Run) -> String {
    if r.stdout.trim().is_empty() {
        return "allow".into();
    }
    let d: Value = serde_json::from_str(r.stdout.trim())
        .unwrap_or_else(|x| panic!("stdout not JSON ({x}): {:?}", r.stdout));
    d["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .unwrap_or_else(|| panic!("no permissionDecision in {d}"))
        .to_string()
}

fn reason(r: &Run) -> String {
    if r.stdout.trim().is_empty() {
        return String::new();
    }
    let d: Value = serde_json::from_str(r.stdout.trim()).unwrap();
    d["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

fn judged(e: &Env, rules: Option<&Path>, payload: &Value) -> Run {
    spawn(e, rules, &[], &payload.to_string())
}

/// `(rule_id, class label without the schema tag, verbs)` of `payload`, via
/// `blastguard shape`. Asserts the analyser Asks (so the test is about an Ask).
fn ask_shape(e: &Env, payload: &Value) -> (String, String, String) {
    let r = spawn(e, None, &["shape"], &payload.to_string());
    assert_eq!(r.code, 0, "shape failed: {:?} {:?}", r.stdout, r.stderr);
    let v: Value = serde_json::from_str(r.stdout.trim()).unwrap();
    assert_eq!(
        v["decision"], "ask",
        "precondition: this must be an Ask: {v}"
    );
    let key = v["shape"].as_str().unwrap();
    let mut it = key.splitn(3, '|');
    let rule = it.next().unwrap().to_string();
    let class = it.next().unwrap().trim_start_matches("s4-").to_string();
    let verbs = it.next().unwrap().to_string();
    (rule, class, verbs)
}

/// One `[[rule]]` that matches exactly the shape of `payload` (single verb
/// shapes only: `tool [sub] [flags..]`).
fn rule_for(e: &Env, payload: &Value, decision: &str, reason: &str) -> String {
    let (rule, class, verbs) = ask_shape(e, payload);
    assert!(!verbs.contains(','), "single-verb shapes only: {verbs}");
    let mut toks = verbs.split(' ');
    let tool = toks.next().unwrap();
    let mut sub = None;
    let mut flags = vec![];
    for t in toks {
        if t.starts_with('-') {
            flags.push(t.to_string());
        } else if t != "?" && sub.is_none() {
            sub = Some(t.to_string());
        }
    }
    let mut m = format!("rule = {rule:?}\nclass = [{class:?}]\ntool = {tool:?}\n");
    if let Some(s) = sub {
        m += &format!("sub = {s:?}\n");
    }
    if !flags.is_empty() {
        m += &format!("flags_all = {}\n", serde_json::to_string(&flags).unwrap());
    }
    format!("[[rule]]\ndecision = {decision:?}\nreason = {reason:?}\n[rule.match]\n{m}\n")
}

fn write_rules(e: &Env, name: &str, text: &str) -> PathBuf {
    let p = e.root.join(name);
    std::fs::write(&p, text).unwrap();
    p
}

fn add_in_wt(e: &Env) -> Value {
    bash(&e.wt, "git add -A")
}
fn add_in_main(e: &Env) -> Value {
    bash(&e.main, "git add -A")
}
fn rm_var(e: &Env) -> Value {
    bash(&e.wt, r#"rm "$f""#)
}

/// The canary: a file with only this rule must turn the worktree
/// `git add -A` Ask into an Allow.
fn canary_rule(e: &Env) -> String {
    rule_for(e, &add_in_wt(e), "allow", "canary: stage in worktree")
}

fn assert_canary_allows(e: &Env, rules: &Path) {
    let r = judged(e, Some(rules), &add_in_wt(e));
    assert_eq!(
        decision(&r),
        "allow",
        "canary: the rule file is not being read. stdout={:?} stderr={:?}",
        r.stdout,
        r.stderr
    );
}

// ------------------------------------------------------------------ control

/// Control (passes today): with no rules file the Asks are exactly as before,
/// whether the env points at a missing path or is unset.
#[test]
fn control_no_rules_file_changes_nothing() {
    let e = env("control");
    for p in [add_in_wt(&e), add_in_main(&e), rm_var(&e)] {
        let unset = judged(&e, None, &p);
        assert_eq!(decision(&unset), "ask", "{:?}", unset.stdout);
        let missing = e.root.join("does-not-exist.toml");
        let r = judged(&e, Some(&missing), &p);
        assert_eq!(decision(&r), "ask", "{:?} {:?}", r.stdout, r.stderr);
        assert_eq!(r.stdout, unset.stdout, "a missing rules file must be inert");
    }
}

// --------------------------------------------------------------------- RED

#[test]
fn allow_rule_turns_ask_into_allow() {
    let e = env("allow");
    let p = write_rules(&e, "r.toml", &canary_rule(&e));
    let r = judged(&e, Some(&p), &add_in_wt(&e));
    assert_eq!(decision(&r), "allow", "{:?} {:?}", r.stdout, r.stderr);
    // a shape the rule does not name stays an Ask
    let r = judged(&e, Some(&p), &rm_var(&e));
    assert_eq!(decision(&r), "ask", "{:?}", r.stdout);
}

#[test]
fn deny_rule_turns_ask_into_deny_with_its_reason() {
    let e = env("deny");
    let p = write_rules(
        &e,
        "r.toml",
        &rule_for(
            &e,
            &rm_var(&e),
            "deny",
            "rm of an unresolved path is forbidden here",
        ),
    );
    let r = judged(&e, Some(&p), &rm_var(&e));
    assert_eq!(decision(&r), "deny", "{:?} {:?}", r.stdout, r.stderr);
    assert!(
        reason(&r).contains("rm of an unresolved path is forbidden here"),
        "the rule's reason must be shown: {}",
        reason(&r)
    );
}

#[test]
fn deny_wins_over_allow_for_the_same_shape() {
    let e = env("both");
    let text = format!(
        "{}{}",
        rule_for(&e, &add_in_wt(&e), "allow", "allow it"),
        rule_for(&e, &add_in_wt(&e), "deny", "deny it")
    );
    let p = write_rules(&e, "r.toml", &text);
    let r = judged(&e, Some(&p), &add_in_wt(&e));
    assert_eq!(decision(&r), "deny", "{:?} {:?}", r.stdout, r.stderr);
}

#[test]
fn deny_rule_applies_to_main_tree_class() {
    let e = env("denymain");
    let p = write_rules(
        &e,
        "r.toml",
        &rule_for(&e, &add_in_main(&e), "deny", "no main"),
    );
    let r = judged(&e, Some(&p), &add_in_main(&e));
    assert_eq!(decision(&r), "deny", "{:?} {:?}", r.stdout, r.stderr);
}

#[test]
fn main_tree_class_ignores_allow_rules() {
    let e = env("mainallow");
    let (_, class, _) = ask_shape(&e, &add_in_main(&e));
    assert_eq!(class, "main-tree", "precondition");
    let text = format!(
        "{}{}",
        canary_rule(&e),
        rule_for(&e, &add_in_main(&e), "allow", "allow main")
    );
    let p = write_rules(&e, "r.toml", &text);
    assert_canary_allows(&e, &p);
    let r = judged(&e, Some(&p), &add_in_main(&e));
    assert_eq!(
        decision(&r),
        "ask",
        "main tree must never be Allow: {:?}",
        r.stdout
    );
}

#[test]
fn unresolved_question_mark_shape_ignores_allow_rules() {
    let e = env("qmark");
    let cmd = bash(&e.wt, "xargs rm");
    let (_, _, verbs) = ask_shape(&e, &cmd);
    assert!(
        verbs.contains('?'),
        "precondition: `?` in verbs, got {verbs}"
    );
    // allow rule naming exactly that shape (tool only; `?` is not a flag/sub)
    let text = format!(
        "{}{}",
        canary_rule(&e),
        rule_for(&e, &cmd, "allow", "allow rm of substitution")
    );
    let p = write_rules(&e, "r.toml", &text);
    assert_canary_allows(&e, &p);
    let r = judged(&e, Some(&p), &cmd);
    assert_ne!(
        decision(&r),
        "allow",
        "a `?` shape must never be Allow: {:?}",
        r.stdout
    );
}

#[test]
fn unknown_key_disables_allow_and_says_why() {
    let e = env("unknownkey");
    let good = write_rules(&e, "good.toml", &canary_rule(&e));
    assert_canary_allows(&e, &good);

    let text = format!("{}\nversion = 1\n", canary_rule(&e));
    let bad = write_rules(&e, "bad.toml", &text);
    let r = judged(&e, Some(&bad), &add_in_wt(&e));
    assert_eq!(decision(&r), "ask", "{:?}", r.stdout);
    assert!(
        r.stderr.contains("version") || r.stderr.to_lowercase().contains("unknown"),
        "the reason must go to stderr: {:?}",
        r.stderr
    );

    // a deny rule in the same defective file stays in force
    let text = format!(
        "{}{}\nversion = 1\n",
        canary_rule(&e),
        rule_for(&e, &rm_var(&e), "deny", "still denied")
    );
    let bad2 = write_rules(&e, "bad2.toml", &text);
    assert_eq!(decision(&judged(&e, Some(&bad2), &add_in_wt(&e))), "ask");
    assert_eq!(decision(&judged(&e, Some(&bad2), &rm_var(&e))), "deny");
}

#[test]
fn unparsable_or_unreadable_file_disables_allow() {
    let e = env("unparsable");
    let good = write_rules(&e, "good.toml", &canary_rule(&e));
    assert_canary_allows(&e, &good);

    let bad = write_rules(&e, "bad.toml", "[[rule\nnot = = toml");
    let r = judged(&e, Some(&bad), &add_in_wt(&e));
    assert_eq!(decision(&r), "ask", "{:?}", r.stdout);
    assert!(
        !r.stderr.trim().is_empty(),
        "silence is not allowed: the reason goes to stderr"
    );

    // a directory where the file should be: exists, cannot be read as a file
    let dir = e.root.join("is-a-dir.toml");
    std::fs::create_dir_all(&dir).unwrap();
    let r = judged(&e, Some(&dir), &add_in_wt(&e));
    assert_eq!(decision(&r), "ask", "{:?}", r.stdout);
    assert!(!r.stderr.trim().is_empty());
}

#[test]
fn shape_not_named_by_any_rule_stays_ask() {
    let e = env("nomatch");
    let p = write_rules(&e, "r.toml", &canary_rule(&e));
    assert_canary_allows(&e, &p);
    let r = judged(&e, Some(&p), &rm_var(&e));
    assert_eq!(decision(&r), "ask", "{:?}", r.stdout);
}

/// (d) the rules file is protected config: a Write tool call to it is never
/// Allow (the rules deployed in the plugin cache under `~/.claude`, and the
/// file BLASTGUARD_ASK_RULES names). The source copy inside a container
/// worktree follows user ruling 2026-10-03 (protected-path rule lifted inside
/// a storage-root checkout).
#[test]
fn rules_files_are_protected_from_writes() {
    let e = env("protected");
    let bundled = e.wt.join("crates/blastguard/rules/ask-rules.toml");
    std::fs::create_dir_all(bundled.parent().unwrap()).unwrap();
    let over = write_rules(&e, "override.toml", "");
    // re-anchored 2026-11-28 (leader ruling: rules source copy follows 2026-10-03)
    // Before: the worktree source copy was in the never-Allow set. After: it
    // follows the storage-root lift (Allow); the deployed copy in the plugin
    // cache (`$HOME/.claude/plugins/cache/...`) takes its place in the set.
    let cached = e
        .home
        .join(".claude/plugins/cache/yukineko/blastguard/0.2.1010/rules/ask-rules.toml");
    std::fs::create_dir_all(cached.parent().unwrap()).unwrap();
    std::fs::write(&cached, "").unwrap();
    let src = json!({
        "hook_event_name": "PreToolUse",
        "session_id": SID,
        "cwd": e.wt.to_str().unwrap(),
        "tool_name": "Write",
        "tool_input": { "file_path": bundled.to_str().unwrap(), "content": "x" },
    });
    let r = judged(&e, Some(&over), &src);
    assert_eq!(
        decision(&r),
        "allow",
        "the source copy in a container worktree follows ruling 2026-10-03: {:?} {:?}",
        r.stdout,
        r.stderr
    );
    for target in [&cached, &over] {
        let payload = json!({
            "hook_event_name": "PreToolUse",
            "session_id": SID,
            "cwd": e.wt.to_str().unwrap(),
            "tool_name": "Write",
            "tool_input": { "file_path": target.to_str().unwrap(), "content": "x" },
        });
        let r = judged(&e, Some(&over), &payload);
        assert_ne!(
            decision(&r),
            "allow",
            "a Write to {target:?} must not be Allow: {:?} {:?}",
            r.stdout,
            r.stderr
        );
    }
    // control: an ordinary file in the same worktree is not blocked by this
    let ok = json!({
        "hook_event_name": "PreToolUse",
        "session_id": SID,
        "cwd": e.wt.to_str().unwrap(),
        "tool_name": "Write",
        "tool_input": { "file_path": e.wt.join("notes.txt").to_str().unwrap(), "content": "x" },
    });
    assert_eq!(decision(&judged(&e, Some(&over), &ok)), "allow");
}

/// `except` / `[[deny]]` / `not_learnable` with `command_regex`, end to end.
#[test]
fn except_and_regex_deny_through_the_hook() {
    let e = env("except");
    let plain = add_in_wt(&e);
    let forced = bash(&e.wt, "git add -A -f");
    let base = rule_for(&e, &plain, "allow", "stage");
    // preconditions: both are Asks; `-f` is part of the second shape
    let _ = ask_shape(&e, &forced);
    // allow rule (tool/sub/-A) + except on the raw command
    let with_except = base.replace(
        "flags_all = [\"-A\"]\n",
        "flags_all = [\"-A\"]\nexcept = [ { command_regex = '\\s-f\\b' } ]\n",
    );
    assert_ne!(
        with_except, base,
        "test setup: the except line was not inserted"
    );
    let p = write_rules(&e, "r.toml", &with_except);
    assert_canary_allows(&e, &p);
    assert_eq!(decision(&judged(&e, Some(&p), &forced)), "ask");

    // a [[deny]] with command_regex
    let p = write_rules(
        &e,
        "d.toml",
        &format!(
            "{base}[[deny]]\nreason = \"no force\"\n[deny.match]\ncommand_regex = '\\s-f\\b'\n"
        ),
    );
    assert_canary_allows(&e, &p);
    let r = judged(&e, Some(&p), &forced);
    assert_eq!(decision(&r), "deny", "{:?} {:?}", r.stdout, r.stderr);
    assert!(reason(&r).contains("no force"), "{}", reason(&r));

    // a regex inside an allow match disables the file's allows
    let bad = base.replace("tool = ", "command_regex = 'git'\ntool = ");
    let p = write_rules(&e, "bad.toml", &bad);
    assert_eq!(decision(&judged(&e, Some(&p), &plain)), "ask");
    assert!(!judged(&e, Some(&p), &plain).stderr.trim().is_empty());
}

/// Positive allow-list (spec "Positive allow-list"): a wrapper / unknown
/// interpreter that is not fully modelled never matches an Allow rule, even a
/// maximally broad one naming only its tool. Each case also asserts the
/// canary, and that the case IS an Ask to begin with.
#[test]
fn unmodelled_wrappers_and_interpreters_never_match_allow() {
    let e = env("allowlist");
    std::fs::write(e.wt.join("x.fish"), "git add -A\n").unwrap();
    let main = e.main.to_str().unwrap().to_string();
    let cases: Vec<String> = vec![
        format!("env -C {main} git add -A"),
        "bash -c \"git add -A\"".into(),
        "find . -exec git add -A \\;".into(),
        "fish x.fish".into(),
    ];
    let all = r#"["worktree","scratchpad","main-tree","home","undetermined","none"]"#;
    for cmd in cases {
        let payload = bash(&e.wt, &cmd);
        let r = spawn(&e, None, &["shape"], &payload.to_string());
        let v: Value = serde_json::from_str(r.stdout.trim())
            .unwrap_or_else(|x| panic!("{cmd}: shape stdout {:?} ({x})", r.stdout));
        assert_eq!(v["decision"], "ask", "precondition for {cmd}: {v}");
        let key = v["shape"].as_str().unwrap();
        let verbs = key.splitn(3, '|').nth(2).unwrap();
        let tool = verbs.split([' ', ',']).next().unwrap();
        let text = format!(
            "{}[[rule]]\ndecision = \"allow\"\nreason = \"broad\"\n[rule.match]\ntool = {tool:?}\nclass = {all}\n",
            canary_rule(&e)
        );
        let p = write_rules(&e, "r.toml", &text);
        assert_canary_allows(&e, &p);
        let r = judged(&e, Some(&p), &payload);
        assert_eq!(
            decision(&r),
            "ask",
            "`{cmd}` (shape {key}) is not a fully modelled shape and must stay an Ask: {:?}",
            r.stdout
        );
    }
}
