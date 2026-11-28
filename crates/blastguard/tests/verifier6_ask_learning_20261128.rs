#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED(verifier round 6, 2026-11-28, against 1a34e6d24): reach paths of the
//! askrules judge (`crates/blastguard/src/askrules/`) and the `s4-` shapes
//! after the round-5 fixes. Written by an independent verifier, not the
//! implementer (CLAUDE.md §2(a)). Black-box: spawns the built binary with a
//! sandbox HOME, a learned table whose ONLY allow entry is the key
//! `blastguard shape` prints for a benign sibling, and (where named) a human
//! rules file via `BLASTGUARD_ASK_RULES`.
//!
//! Fixtures live in a fresh `tempfile` directory `<root>` =
//! `/private/tmp/claude-v6-*` (re-anchored 2026-11-28, leader ruling);
//! `<root>/-proj/<SID>/scratchpad` is classed `s4-scratchpad`; a linked git
//! worktree `<root>/repo/.claude/worktrees/wt` of the main tree `<root>/repo`
//! is created per test.
//!
//! Each test first checks its preconditions (the probe is NOT Allow without the
//! table; the sibling Asks; the entry is live for the sibling), so a GREEN
//! cannot come from a drifted fixture.
//!
//! What these tests do NOT prove: that the learner would actually promote the
//! sibling; that the probe's harm happens at run time (nothing is executed);
//! anything about shapes not listed here.

#[path = "support/bounded.rs"]
mod bounded;

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const SID: &str = "5d0c1f0e-aaaa-4bbb-8ccc-0000000000d6";

struct Env {
    _dir: tempfile::TempDir,
    _home: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    wt: PathBuf,
    main: PathBuf,
    sp: PathBuf,
}

fn git(dir: &Path, args: &[&str]) {
    let st = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?} in {}", dir.display());
}

/// HOME outside every temp root (cargo's target tmp dir), like production.
fn env(name: &str) -> Env {
    env_with(name, false)
}

/// HOME inside the temp root (`/private/tmp/...`): the configuration in which
/// a scratchpad / worktree `rm -rf` is an Ask (verifier5 `probe_home_location`).
fn env_home_in_tmp(name: &str) -> Env {
    env_with(name, true)
}

fn env_with(name: &str, home_in_tmp: bool) -> Env {
    // fixture re-anchored 2026-11-28 (leader ruling): fixed container / scratchpad positions match the real layout
    // (root = /private/tmp/claude-<n>, scratchpad = <root>/-proj/<SID>/scratchpad)
    let dir = tempfile::Builder::new()
        .prefix(&format!("claude-v6-{name}-"))
        .tempdir_in("/private/tmp")
        .unwrap();
    let root = dir.path().to_path_buf();
    let home_dir = if home_in_tmp {
        tempfile::Builder::new()
            .prefix("home-")
            .tempdir_in(&root)
            .unwrap()
    } else {
        tempfile::Builder::new()
            .prefix(&format!("v6-home-{name}-"))
            .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
            .unwrap()
    };
    let home = home_dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("approvals")).unwrap();
    let main = root.join("repo");
    std::fs::create_dir_all(main.join("src")).unwrap();
    std::fs::write(main.join("src").join("lib.rs"), "x").unwrap();
    git(&main, &["init", "-q"]);
    git(&main, &["add", "src"]);
    git(&main, &["commit", "-q", "-m", "i"]);
    // fixture re-anchored 2026-11-28 (leader ruling): fixed container / scratchpad positions match the real layout
    // (Claude Code's container `<main>/.claude/worktrees`, NOT `.harness-worktrees`:
    // the latter is a worktree STORAGE root where the rm detector itself Allows
    // `rm -rf build` (src/detect/rm.rs `worktree_confined`), so the siblings
    // would no longer Ask.)
    let wt = main.join(".claude").join("worktrees").join("wt");
    std::fs::create_dir_all(main.join(".claude").join("worktrees")).unwrap();
    git(&main, &["worktree", "add", "-q", wt.to_str().unwrap()]);
    // fixture re-anchored 2026-11-28 (leader ruling): Scratchpad is the session's own scratchpad only
    let sp = root.join("-proj").join(SID).join("scratchpad");
    std::fs::create_dir_all(&sp).unwrap();
    Env {
        _dir: dir,
        _home: home_dir,
        root,
        home,
        wt,
        main,
        sp,
    }
}

fn spawn(
    e: &Env,
    table: &Path,
    rules: Option<&Path>,
    args: &[&str],
    stdin: &str,
) -> (i32, String, String) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    c.args(args)
        .env("HOME", &e.home)
        .env("BLASTGUARD_BLOCK_LOG_DIR", e.root.join("blocklog"))
        .env("BLASTGUARD_RELAY_DIR", e.root.join("relay"))
        .env("BLASTGUARD_APPROVALS_DIR", e.root.join("approvals"))
        .env("BLASTGUARD_LEARNED_TABLE", table)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("BLASTGUARD_ASK")
        .env_remove("BLASTGUARD_ASK_RULES")
        .env_remove("CLAUDE_PROJECT_DIR")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(r) = rules {
        c.env("BLASTGUARD_ASK_RULES", r);
    }
    let out = bounded::run_bounded(&mut c, stdin.as_bytes(), bounded::HOOK_DEADLINE);
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn no_table(e: &Env) -> PathBuf {
    e.root.join("no-table.json")
}

fn decide_with(e: &Env, table: &Path, rules: Option<&Path>, p: &Value) -> String {
    let (_, out, err) = spawn(e, table, rules, &[], &p.to_string());
    if out.trim().is_empty() {
        return "allow".to_string();
    }
    let d: Value = serde_json::from_str(out.trim())
        .unwrap_or_else(|x| panic!("stdout not JSON ({x}): {out:?} stderr={err:?}"));
    d["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .unwrap_or_else(|| panic!("no permissionDecision in {d}"))
        .to_string()
}

fn bash(cwd: &Path, command: &str) -> Value {
    json!({
        "hook_event_name": "PreToolUse",
        "session_id": SID,
        "cwd": cwd,
        "tool_name": "Bash",
        "tool_input": { "command": command },
    })
}

fn shape_with(e: &Env, rules: Option<&Path>, p: &Value) -> Value {
    let (code, out, err) = spawn(e, &no_table(e), rules, &["shape"], &p.to_string());
    assert_eq!(code, 0, "shape failed: {out:?} {err:?}");
    serde_json::from_str(out.trim()).unwrap()
}

fn shape(e: &Env, p: &Value) -> Value {
    shape_with(e, None, p)
}

fn table_with(e: &Env, k: &str) -> PathBuf {
    let p = e.root.join("t.json");
    let entry = json!({"shape": k, "decision": "allow", "approvals": 50, "declines": 0,
                       "sessions": 20, "samples": []});
    std::fs::write(&p, json!({"version": 1, "entries": [entry]}).to_string()).unwrap();
    p
}

fn cmd_of(p: &Value) -> &str {
    p["tool_input"]["command"].as_str().unwrap_or("")
}

/// Learn `sibling` allow; `probe` must not become Allow.
fn assert_no_ride(e: &Env, rules: Option<&Path>, sibling: &Value, probe: &Value) {
    let before = decide_with(e, &no_table(e), rules, probe);
    assert_ne!(
        before,
        "allow",
        "precondition: probe {:?} must not be Allow without a table",
        cmd_of(probe)
    );
    let sib = shape_with(e, rules, sibling);
    assert_eq!(
        sib["decision"],
        "ask",
        "precondition: sibling {:?} must Ask: {sib}",
        cmd_of(sibling)
    );
    let k = sib["shape"].as_str().unwrap().to_string();
    let t = table_with(e, &k);
    assert_eq!(
        decide_with(e, &t, rules, sibling),
        "allow",
        "control: sibling {:?} with its own allow entry {k}",
        cmd_of(sibling)
    );
    let after = decide_with(e, &t, rules, probe);
    assert_ne!(
        after,
        "allow",
        "probe {:?} was {before} without a table but Allow once the sibling {:?} was learned; \
         learned key {k}, probe shape {}",
        cmd_of(probe),
        cmd_of(sibling),
        shape_with(e, rules, probe)
    );
}

// ------------------------------------------------------- assignment via `for`

/// `for PATH in <dir>; do git add -A; done` runs `<dir>/git` (any program):
/// the `for` variable is an assignment to an exported variable, but neither a
/// prefix nor a standalone assignment, so the round-5 `?` does not fire.
/// Observed at 1a34e6d24: key `git-whole-tree-stage|s4-worktree|git add -A`,
/// the plain command's key. (`export` / `declare -x` / `readonly` / `;`
/// assignments / `case` / `if` / `while` all carry `?`: observed.)
#[test]
fn r6_for_loop_variable_path_rides_git_add_allow() {
    let e = env("forpath");
    let bin = e.sp.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("git"), "#!/bin/sh\necho any program\n").unwrap();
    let sibling = bash(&e.wt, "git add -A");
    let probe = bash(
        &e.wt,
        &format!("for PATH in {}; do git add -A; done", bin.display()),
    );
    assert_no_ride(&e, None, &sibling, &probe);
}

/// `for HOME in <dir>; do git commit -a; done`: git reads `<dir>/.gitconfig`,
/// whose `core.fsmonitor` is a program git runs (same effect as the round-5
/// `HOME=<dir> git ..`). Observed key `git-whole-tree-stage|s4-worktree|git
/// commit -a`, the plain command's key.
#[test]
fn r6_for_loop_variable_home_rides_git_commit_allow() {
    let e = env("forhome");
    let fh = e.sp.join("fh");
    std::fs::create_dir_all(&fh).unwrap();
    std::fs::write(
        fh.join(".gitconfig"),
        format!("[core]\n\tfsmonitor = {}/evil\n", e.sp.display()),
    )
    .unwrap();
    let sibling = bash(&e.wt, "git commit -a");
    let probe = bash(
        &e.wt,
        &format!("for HOME in {}; do git commit -a; done", fh.display()),
    );
    assert_no_ride(&e, None, &sibling, &probe);
}

// ------------------------------------------------------- ANSI-C quoting

/// CONDITIONAL (a human rules file with no restrictive `command_regex`):
/// `$'\x2dA'` is `-A` to the shell, but the evaluator does not decode the
/// escape, the word is undetermined, and an undetermined operand adds nothing
/// to the key (R1), not even `?`, although it stands where an option can be.
/// Observed at 1a34e6d24: `git add $'\x2dA'` / `$'\055A'` and `git add
/// $'\x66oo'` (= `git add foo`) are all Ask with key
/// `unknown|s4-worktree|git add`; `git commit $'\x2d-amend'` and `git commit
/// $'\x66oo'` both `unknown|s4-worktree|git commit`. (`$'-A'`, `-\A`, `"-"A`
/// keep `-A`: observed.)
///
/// With the BUNDLED rules this is NOT reachable (control below, observed): the
/// bundled `not_learnable` entries carry `command_regex`, and undetermined
/// quote-stripped words count as matched, so no shape with such a word is
/// learnable. The positive allow-list itself (the primary defence) does not
/// stop it: with a valid human file that has no `command_regex` entry, the
/// sibling's learned allow rides the probe.
#[test]
fn r6_ansi_c_escape_hides_an_option_from_the_shape() {
    let e = env("ansic");
    let plain = e.root.join("rules-plain.toml");
    std::fs::write(
        &plain,
        "[[deny]]\nreason = \"no force push\"\n[deny.match]\ntool = \"git\"\nsub = \"push\"\n\
         flags_any = [\"--force\", \"-f\"]\n",
    )
    .unwrap();
    // Liveness control (verifier round 7): with the same rules file a learned
    // allow IS honoured for an eligible line, so the "neither is Allow"
    // asserts below cannot come from a rules file that poisons every Allow.
    let live = bash(&e.wt, "git add -A");
    let lk = shape(&e, &live)["shape"].as_str().unwrap().to_string();
    assert_eq!(
        decide_with(&e, &table_with(&e, &lk), Some(&plain), &live),
        "allow",
        "control: `git add -A` under its own learned entry {lk} with the rules file"
    );
    for (sibling, probe) in [
        (r"git add $'\x66oo'", r"git add $'\x2dA'"),
        (r"git add $'\x66oo'", r"git add $'\055A'"),
        (r"git commit $'\x66oo'", r"git commit $'\x2d-amend'"),
    ] {
        let (s, p) = (bash(&e.wt, sibling), bash(&e.wt, probe));
        // control: with the bundled rules the learned allow is not honoured
        let k = shape(&e, &s)["shape"].as_str().unwrap().to_string();
        assert_eq!(
            decide_with(&e, &table_with(&e, &k), None, &p),
            "ask",
            "control: bundled rules keep `{probe}` an Ask"
        );
        // Re-anchored (round 7): the sibling is itself a `$'..'` spelling, so
        // the structural gate (static literal words only) refuses to Allow
        // it, and `assert_no_ride`'s control (the sibling's own entry allows)
        // cannot hold. No eligible sibling has this key, so the property is
        // stated directly: with a table holding the key the probe and the
        // sibling share, NEITHER is Allow.
        assert_eq!(shape(&e, &p)["shape"].as_str().unwrap(), k, "same key");
        let t = table_with(&e, &k);
        assert_ne!(
            decide_with(&e, &t, Some(&plain), &p),
            "allow",
            "probe `{probe}` rides the learned entry {k}"
        );
        assert_ne!(
            decide_with(&e, &t, Some(&plain), &s),
            "allow",
            "sibling `{sibling}` rides the learned entry {k}"
        );
    }
}

// ------------------------------------------------------- `..` after a symlink

/// The kernel resolves `<link>/..` PHYSICALLY (the parent of the link's
/// target), but `class_of_path` normalises `..` LEXICALLY before it
/// canonicalises (`absolute_of` -> `exclude::normalize`, then `realpath_opt`
/// on the already-collapsed spelling), so `<sp>/hlink/../hooks` with `hlink ->
/// ~/.claude/hooks` (really `~/.claude/hooks`) is classed `s4-scratchpad`.
/// Direct property, default HOME: observed `scoped-destructive-confined|
/// s4-scratchpad|rm -f -r` for the protected-config target and for the main
/// tree target `<sp>/mlink/../notes` (`mlink -> <main>/src`).
#[test]
fn r6_dotdot_after_a_symlink_is_classed_by_the_collapsed_spelling() {
    let e = env("dotdotclass");
    fixtures(&e);
    let mut bad = vec![];
    for probe in [
        format!("rm -rf {}/hlink/../hooks", e.sp.display()),
        format!("rm -rf {}/mlink/../notes", e.sp.display()),
        format!("rm -rf {}/wlink/../notes", e.wt.display()),
    ] {
        let s = shape(&e, &bash(&e.wt, &probe));
        let k = s["shape"].as_str().unwrap();
        let class = k.split('|').nth(1).unwrap_or("");
        if ["s4-scratchpad", "s4-worktree", "s4-none"].contains(&class) {
            bad.push(format!("`{probe}` => {} {k}", s["decision"]));
        }
    }
    assert!(
        bad.is_empty(),
        "a target reached through `<symlink>/..` (protected config / the main tree) is classed \
         as an Allow-eligible place:\n{}",
        bad.join("\n")
    );
}

/// CONDITIONAL (HOME inside /private/tmp, where a plain scratchpad / worktree
/// `rm -rf` is an Ask, as in verifier5): the learned allow of a plain
/// scratchpad `rm -rf` rides `rm -rf <sp>/hlink/../hooks` (deletes
/// `~/.claude/hooks`) and `rm -rf <sp>/mlink/../notes` (deletes the main
/// tree's untracked `notes`); the learned allow of a worktree `rm -rf build`
/// rides `rm -rf <wt>/wlink/../notes`. The round-5 symlink fix (no `..`) does
/// not cover this spelling.
#[test]
fn r6_dotdot_after_a_symlink_rides_placed_rm_allow() {
    let e = env_home_in_tmp("dotdotride");
    fixtures(&e);
    let sp = e.sp.display();
    for probe in [
        format!("rm -rf {sp}/hlink/../hooks"),
        format!("rm -rf {sp}/mlink/../notes"),
    ] {
        assert_no_ride(
            &e,
            None,
            &bash(&e.wt, &format!("rm -rf {sp}/x")),
            &bash(&e.wt, &probe),
        );
    }
    assert_no_ride(
        &e,
        None,
        &bash(&e.wt, &format!("rm -rf {}/build", e.wt.display())),
        &bash(&e.wt, &format!("rm -rf {}/wlink/../notes", e.wt.display())),
    );
}

// ------------------------------------------------------- the override file

/// Invariant (d): the `BLASTGUARD_ASK_RULES` target is protected config;
/// shell writes to it and removing / renaming an ancestor are never Allow.
/// Observed Allow at 1a34e6d24 (no table): the DIRECTORY as the `cp` / `mv`
/// destination (`cp <f> <sp>/r`, `cp -f`, `mv <f> <sp>/r`: the file lands at
/// `<sp>/r/ask-rules.toml`, replacing the rules); another spelling of the
/// file or directory: through `<sp>/d/zlink/..` (`zlink -> <sp>/z`), and in
/// upper case on a case-insensitive file system (macOS default).
/// (`cp <f> <sp>/r/`, `mv <f> <sp>/r/`, `cp -R <d>/ <sp>/r`, `ln -sf <f>
/// <override>` are refused: observed.)
#[test]
fn r6_rules_override_reached_through_its_directory_or_another_spelling_is_not_allow() {
    let e = env("ovr6");
    fixtures(&e);
    let rules = e.sp.join("r").join("ask-rules.toml");
    std::fs::write(&rules, PROBE_RULES).unwrap();
    let evil = e.sp.join("evil").join("ask-rules.toml");
    let sp = e.sp.display();
    let ev = evil.display();
    // control: the spelled file itself is protected
    let control = format!("cp {ev} {}", rules.display());
    assert_ne!(
        decide_with(&e, &no_table(&e), Some(&rules), &bash(&e.wt, &control)),
        "allow",
        "control: `{control}`"
    );
    let mut probes = vec![
        format!("cp {ev} {sp}/r"),
        format!("cp -f {ev} {sp}/r"),
        format!("mv {ev} {sp}/r"),
        format!("rm -f {sp}/d/zlink/../r/ask-rules.toml"),
        format!("mv {sp}/d/zlink/../r {sp}/r2"),
    ];
    // Case variants only where the file system folds case (observed, not assumed).
    let case_insensitive = e.sp.join("R").join("ASK-RULES.toml").exists();
    if case_insensitive {
        probes.extend([
            format!("cp {ev} {sp}/R/ASK-RULES.toml"),
            format!("rm -f {sp}/R/ASK-RULES.toml"),
            format!("rm -rf {sp}/R"),
            format!("mv {sp}/R {sp}/r2"),
        ]);
    } else {
        println!("case-sensitive file system: case variants not probed");
    }
    let mut bad = vec![];
    for c in probes {
        let got = decide_with(&e, &no_table(&e), Some(&rules), &bash(&e.wt, &c));
        if got == "allow" {
            bad.push(c);
        }
    }
    assert!(
        bad.is_empty(),
        "these replace / remove the BLASTGUARD_ASK_RULES file but are Allow:\n{}",
        bad.join("\n")
    );
}

// ------------------------------------------------------- shell script files

/// Ruling 2026-11-28 (round 6): a `bash` / `sh` script FILE run is never
/// learned-Allowed (the content may change after learning). GUARD at the
/// binary level (observed GREEN at 1a34e6d24: the shapes are classed
/// `s4-undetermined`, so they are ineligible for that reason); the judge-level
/// RED with an eligible class is in ask_rules_unit_20261128.rs.
#[test]
fn r6_shell_script_file_run_is_never_learned_allow() {
    let e = env("shfile");
    fixtures(&e);
    let mut bad = vec![];
    for c in [
        "bash ./run.sh".to_string(),
        "sh ./run.sh".to_string(),
        format!("bash {}/run.sh", e.sp.display()),
        format!("bash -e {}/run.sh", e.sp.display()),
        format!("bash < {}/run.sh", e.sp.display()),
    ] {
        let p = bash(&e.wt, &c);
        assert_eq!(
            decide_with(&e, &no_table(&e), None, &p),
            "ask",
            "precondition: `{c}` Asks without a table"
        );
        let k = shape(&e, &p)["shape"].as_str().unwrap().to_string();
        let got = decide_with(&e, &table_with(&e, &k), None, &p);
        if got == "allow" {
            bad.push(format!("`{c}` is Allow under its own learned entry {k}"));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

// ------------------------------------------------------- detector (not askrules)

/// DETECTOR, outside askrules (no table involved): `rm -f <sp>/hlink/../
/// settings.json` with `hlink -> ~/.claude/hooks` deletes
/// `~/.claude/settings.json` and is a plain Allow (observed at 1a34e6d24,
/// shape `allow|s4-scratchpad|rm -f`). Control: the direct spelling is not
/// Allow.
#[test]
#[ignore = "detector, filed separately"]
fn r6_detector_symlink_dotdot_removes_protected_config_without_ask() {
    let e = env("detprot");
    fixtures(&e);
    let control = format!("rm -f {}/.claude/settings.json", e.home.display());
    assert_ne!(
        decide_with(&e, &no_table(&e), None, &bash(&e.wt, &control)),
        "allow",
        "control: `{control}`"
    );
    let probe = format!("rm -f {}/hlink/../settings.json", e.sp.display());
    assert_ne!(
        decide_with(&e, &no_table(&e), None, &bash(&e.wt, &probe)),
        "allow",
        "`{probe}` deletes ~/.claude/settings.json (protected config) through `<symlink>/..`"
    );
}

/// DETECTOR, outside askrules: brace expansion hides the destructive option.
/// `git reset {--hard,HEAD}` is `git reset --hard HEAD` to bash and zsh, a
/// detector Deny when spelled out; it is a plain Allow (observed at 1a34e6d24,
/// shape `allow|s4-worktree|git reset`). `git clean {-f,-x}` (= `git clean -f
/// -x`) is Allow while `git clean -f` Asks.
#[test]
#[ignore = "detector, filed separately"]
fn r6_detector_brace_expansion_hides_a_destructive_option() {
    let e = env("detbrace");
    // A tracked change (what `reset --hard` discards) and an untracked file
    // (what `clean -f` deletes); without them both are recoverable no-ops.
    std::fs::write(e.wt.join("src").join("lib.rs"), "changed").unwrap();
    std::fs::write(e.wt.join("untracked.txt"), "not in git").unwrap();
    let mut bad = vec![];
    for (spelled, braced) in [
        ("git reset --hard HEAD", "git reset {--hard,HEAD}"),
        ("git clean -f", "git clean {-f,-x}"),
    ] {
        let d = decide_with(&e, &no_table(&e), None, &bash(&e.wt, spelled));
        assert_ne!(d, "allow", "control: `{spelled}` is not Allow");
        let got = decide_with(&e, &no_table(&e), None, &bash(&e.wt, braced));
        if got == "allow" {
            bad.push(format!("`{braced}` is Allow; `{spelled}` is {d}"));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

// ------------------------------------------------------- poisons_allow

/// GUARD (false positives of `poisons_allow`): a rules file that uses only the
/// documented format (spec "Format" and "Allow, except ...": `[[rule]]` allow
/// with `except`, `[[rule]]` deny, `[[deny]]`, `[[not_learnable]]`, an inline
/// `match` table, comments) poisons nothing, keeps its allow rules and leaves
/// a learned allow honoured. A defective ALLOW rule disables only its file's
/// allows: a learned allow next to it is still honoured. Only a lost
/// restriction poisons.
#[test]
fn r6_poisons_allow_spares_a_valid_rules_file_and_a_defective_allow_rule() {
    use blastguard::askrules::{judge, Facts, Judgement, RuleSet};
    use harness_core::asklearn::{Entry, LearnedTable, LearnedVerdict, Shape, TargetClass};
    let valid = r#"
# comment
[[rule]]
decision = "allow"
reason   = "stage in a worktree"
[rule.match]
tool = "git"
sub  = "add"
class = ["worktree", "scratchpad"]
flags_all = ["-A"]
except = [
  { command_regex = '\s-f\b' },
  { class = ["scratchpad"], verbs_regex = 'push' },
]

[[rule]]
decision = "deny"
reason = "never force-delete"
match = { tool = "git", sub = "branch", flags_any = ["-D"] }

[[deny]]
reason = "wrapper that cd's elsewhere"
[deny.match]
command_regex = '\benv\s+-[^ ]*[CS]'

[[not_learnable]]
reason = "inline shell"
[not_learnable.match]
command_regex = '\b(ba|z|fi)?sh\s+-c\b'
"#;
    let shape = Shape {
        rule_id: "git-whole-tree-stage".into(),
        target_class: TargetClass::Worktree,
        verbs: vec!["git commit -a".into()],
    };
    let facts = Facts::from_shape(&shape, false).with_command("git commit -a");
    let learned = RuleSet::from_learned(&LearnedTable {
        version: 1,
        entries: vec![Entry {
            shape: shape.key(),
            decision: LearnedVerdict::Allow,
            approvals: 50,
            declines: 0,
            sessions: 20,
            samples: vec![],
        }],
    });
    assert!(
        matches!(judge(&facts, &[&learned]), Judgement::Allow(_)),
        "control: the learned entry alone allows"
    );
    let rs = RuleSet::from_toml(valid);
    assert!(rs.warnings().is_empty(), "{:?}", rs.warnings());
    assert!(rs.allow_enabled());
    assert!(!rs.poisons_allow(), "a valid file poisons");
    assert!(matches!(
        judge(&facts, &[&rs, &learned]),
        Judgement::Allow(_)
    ));
    let add = Facts::from_shape(
        &Shape {
            verbs: vec!["git add -A".into()],
            ..shape.clone()
        },
        false,
    )
    .with_command("git add -A");
    assert!(
        matches!(judge(&add, &[&rs]), Judgement::Allow(_)),
        "the valid file's own allow rule"
    );
    // a defective allow rule: its file's allows go, a learned allow stays
    let bad_allow = RuleSet::from_toml(
        "[[rule]]\ndecision = \"allow\"\nreason = \"x\"\nnote = \"unknown\"\n[rule.match]\ntool = \"git\"\n",
    );
    assert!(!bad_allow.allow_enabled());
    assert!(!bad_allow.poisons_allow(), "{:?}", bad_allow.warnings());
    assert!(matches!(
        judge(&facts, &[&bad_allow, &learned]),
        Judgement::Allow(_)
    ));
    // control: a lost restriction poisons (spec invariant (c))
    let bad_deny = RuleSet::from_toml(
        "[[deny]]\nreason = \"x\"\nnote = \"unknown\"\n[deny.match]\ntool = \"git\"\n",
    );
    assert!(bad_deny.poisons_allow());
    assert_eq!(judge(&facts, &[&bad_deny, &learned]), Judgement::NoMatch);
}

// ------------------------------------------------------- measurement only

/// The measured probe lines (see [`probe_dump`]).
const PROBES: &str = r#"
W rm -rf $SP/evil
W rm -rf $SP/z
W rm -rf $SP/nonexist
W rm -rf $SP/mlink/../notes
W rm -rf $SP/hlink/..
W rm -rf $WT/build
W rm -rf $WT/wlink/../notes
W rm -rf $WT/wlink/../src
W git commit $'\x2d-amend'
W git commit $'\x66oo'
W git rm $'\x2dr' -f src
W git rm -f src
W git add $'\x66oo'
W git add $'\x2dA'
W git add $'\055A'
W git reset HEAD
W git reset {--hard,HEAD}
W git reset --hard HEAD
W git checkout main
W git checkout {-f,main}
W git branch x
W git branch {-D,x}
W git branch -d x
W git clean -f
W git clean {-f,-x}
W git stash drop
W git stash {drop,}
W rm -rf $SP/hlink/../hooks
W rm -rf $SP/x
W cp $SP/evil/ask-rules.toml $SP/d/zlink/../r/ask-rules.toml
W cp $SP/evil/ask-rules.toml $SP/R/ASK-RULES.toml
W mv $SP/evil/ask-rules.toml $SP/r
W mv -f $SP/evil/ask-rules.toml $SP/r/
W cp -f $SP/evil/ask-rules.toml $SP/r
W cp $SP/evil/ask-rules.toml $SP/r
W mv $SP/x $WT/wlink/../notes
W cp $SP/x $WT/wlink/../notes
W cp $SP/x $SP/mlink/../notes
W bash -e $SP/run.sh
W bash < $SP/run.sh
W bash <<'EOF'\necho hi\nEOF
W bash $SP/run.sh > $SP/out
W case x in *) PATH=$SP/bin;; esac; git add -A
W true && PATH=$SP/bin; git add -A
W while PATH=$SP/bin; false; do :; done; git add -A
W for PATH in $SP/bin; do :; done; git add -A
W for HOME in $SP; do git commit -a; done
W git commit -a
HW rm -rf $SP/x && git add -A
HW rm -rf $SP/hlink/../hooks && git add -A
"#;

/// The rules file the probes run with (makes `<sp>/r/ask-rules.toml` the
/// `BLASTGUARD_ASK_RULES` override; matches nothing).
const PROBE_RULES: &str = r#"
[[not_learnable]]
reason = "probe: never matches"
[not_learnable.match]
command_regex = 'ZZZNEVERMATCHES'
"#;

/// Measurement helper (asserts nothing): prints verdict and shape of every
/// line of [`PROBES`]. Line format `<where> <cmd>`; where = `W` (linked
/// worktree), `M` (main tree), `S` (scratchpad); prefix `H` (`HW`, `HM`, `HS`)
/// runs with HOME inside /private/tmp. `$ROOT`, `$SP`, `$WT`, `$MAIN`,
/// `$HOMEDIR` are substituted, `\n` is a newline.
#[test]
#[ignore]
fn probe_dump() {
    let text = PROBES;
    let rules_text = Some(PROBE_RULES);
    for home_in_tmp in [false, true] {
        let e = env_with("probe", home_in_tmp);
        fixtures(&e);
        let rules = rules_text.as_ref().map(|t| {
            let p = e.sp.join("r").join("ask-rules.toml");
            std::fs::write(&p, t).unwrap();
            p
        });
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let (where_, cmd) = line.split_once(' ').unwrap();
            let h = where_.starts_with('H');
            if h != home_in_tmp {
                continue;
            }
            let w = where_.trim_start_matches('H');
            let cmd = cmd
                .replace("\\n", "\n")
                .replace("$ROOT", e.root.to_str().unwrap())
                .replace("$SP", e.sp.to_str().unwrap())
                .replace("$WT", e.wt.to_str().unwrap())
                .replace("$MAIN", e.main.to_str().unwrap())
                .replace("$HOMEDIR", e.home.to_str().unwrap());
            let cwd = match w {
                "M" => &e.main,
                "S" => &e.sp,
                _ => &e.wt,
            };
            let p = bash(cwd, &cmd);
            let s = shape_with(&e, rules.as_deref(), &p);
            let d = decide_with(&e, &no_table(&e), rules.as_deref(), &p);
            println!("{where_} {cmd:<70} => {d:<5} {}", s["shape"]);
        }
    }
}

/// The links and files the probes refer to.
fn fixtures(e: &Env) {
    let claude = e.home.join(".claude");
    std::fs::create_dir_all(claude.join("hooks")).unwrap();
    std::fs::write(claude.join("settings.json"), "{}").unwrap();
    std::fs::write(e.main.join("notes"), "untracked").unwrap();
    std::fs::create_dir_all(e.sp.join("z")).unwrap();
    std::fs::create_dir_all(e.sp.join("d")).unwrap();
    std::fs::create_dir_all(e.sp.join("r")).unwrap();
    std::fs::write(e.sp.join("r").join("ask-rules.toml"), "").unwrap();
    std::fs::create_dir_all(e.sp.join("evil")).unwrap();
    std::fs::write(e.sp.join("evil").join("ask-rules.toml"), "").unwrap();
    // scratchpad/mlink -> <main>/src : `mlink/..` is the main tree
    std::os::unix::fs::symlink(e.main.join("src"), e.sp.join("mlink")).unwrap();
    // scratchpad/hlink -> ~/.claude/hooks : `hlink/..` is ~/.claude
    std::os::unix::fs::symlink(claude.join("hooks"), e.sp.join("hlink")).unwrap();
    // scratchpad/d/zlink -> scratchpad/z : `d/zlink/../r` is scratchpad/r
    std::os::unix::fs::symlink(e.sp.join("z"), e.sp.join("d").join("zlink")).unwrap();
    // worktree/wlink -> <main>/src
    std::os::unix::fs::symlink(e.main.join("src"), e.wt.join("wlink")).unwrap();
    std::fs::create_dir_all(e.sp.join("bin")).unwrap();
    std::fs::write(e.sp.join("run.sh"), "#!/bin/sh\necho one\n").unwrap();
    std::fs::write(e.wt.join("run.sh"), "#!/bin/sh\necho one\n").unwrap();
}
