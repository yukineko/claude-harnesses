#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false positive, observed 2026-11-15): a worker's
//! `D=<dir>; for f in decomp decomp.routed; do jq '.tasks|=map(.x)'
//! $D/$f.json > $D/$f.tmp && mv $D/$f.tmp $D/$f.json; done` was Ask (Deny in a
//! subagent)
//! with "'> $D/$f.tmp': a command that may relink it runs first and the path
//! cannot be placed, so blastguard cannot tell which file the write really lands
//! on". `$D` is a literal assignment and `$f` iterates a literal word list, so
//! every value is text-determined (user ruling: text-determined values are
//! resolved and judged).
//!
//! Reproduced 2026-11-15 on blastguard 0.2.611 (scratch HOME, cwd=HOME/wt, binary
//! run directly with PreToolUse JSON on stdin). Observed (main / subagent):
//!   original (loop, literal list, `$D/$f.tmp` write + mv)  ask  / deny  same reason
//!   same with the write target literal (`.../lit.tmp`)      ask  / deny  (reason moves
//!       to the mv destination `$D/$f.json` of "a relinking command")
//!   same with NO loop var in any path (literal list, `lit` paths)  allow / allow
//!   no loop, `$D` literal assignment, `> $D/a.tmp && mv ...`  allow / allow
//!   loop with `$f` write and `rm`/`cp` instead of mv          deny / deny ("destroys the
//!       file's current contents", `$f.tmp` shown unexpanded)
//!   loop `$f` write, no mv at all                             deny / deny (same)
//!   loop `$f` write, then an UNRELATED `mv $D/z $D/y`          ask  / deny  same reason as original
//!   loop over `$(ls)` with `$f` write + mv                     ask  / deny  same reason as original
//!   `ln -s /etc $D/lnk` then write+mv through lnk              deny / deny (system dir)
//! The message shows `$f` unexpanded inside an otherwise resolved path while `$D`
//! IS resolved, so the discriminating variable is the for-loop variable, not the
//! `mv` (an unrelated mv in the same line still triggers it) and not `$D`.
//!
//! Two readings: (1) real defect: the loop word list is a literal, so `$f` takes
//! the finitely many values `decomp`, `decomp.routed`; each path is statically
//! placeable (inside the scratch dir) and blastguard should judge each, but the
//! loop variable is left unresolved and the path is treated as unplaceable;
//! (2) intended: a path that an earlier `mv` in the loop could have turned into a
//! symlink is genuinely undetermined. Tell apart: reading (2) needs the mv to
//! matter, but an `mv` of unrelated paths (`mv $D/z $D/y`) fires the identical
//! reason, `$D` (also a variable) is resolved fine, and the identical line with
//! no loop variable is allow; the mv here moves `$D/<f>.tmp` onto `$D/<f>.json`,
//! never creating a link at `$D`. So the blocker is the unresolved `$f`
//! -> reading (1). The controls keep the restriction where a value really is
//! undetermined (`$(ls)`) or really lands in a system directory (symlink).
//!
//! Asserted: for the original command shape, main == "allow" and subagent ==
//! "allow". Controls (must stay != "allow" in main): a loop over `$(ls)` with the
//! same body, and a symlink to /etc created earlier then written through.
//!
//! DOES NOT PROVE: that `for f in <literal list>` is the only way to make `$f`
//! static (`for f in "$@"`, globs, brace expansion), that the `$(ls)` control
//! would remain Ask rather than Deny after a fix (only != allow is asserted),
//! that a literal list containing a value that goes through an earlier-created
//! symlink is still refused (not probed), that the loop-without-mv deny
//! ("destroys the file's current contents", also with `$f` unexpanded) is the
//! same defect (observed the same shape, not asserted here), or that the
//! `.tmp` files being absent vs present changes the verdict.
//!
//! RED written by an independent test author, not the implementer.

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
        "bg-loopmv-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt").join("run")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "loopmv-20261115",
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
        return ("allow".into(), String::new());
    }
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let h = &v["hookSpecificOutput"];
    (
        h["permissionDecision"].as_str().unwrap_or("?").to_string(),
        h["permissionDecisionReason"]
            .as_str()
            .unwrap_or("")
            .to_string(),
    )
}

/// `D` is assigned a literal path inside the scratch cwd; the loop word list is
/// literal; every write lands inside `D`.
fn loop_cmd(home: &Home, list: &str) -> String {
    let d = home.0.join("wt").join("run");
    let d = d.display();
    format!(
        "D={d}; for f in {list}; do jq '.tasks|=map(.x)' $D/$f.json > $D/$f.tmp && mv $D/$f.tmp $D/$f.json; done"
    )
}

#[test]
fn literal_list_for_loop_tmp_then_mv_is_allowed() {
    let home = unique_home("fp");
    let cmd = loop_cmd(&home, "decomp decomp.routed");
    let (m, mr) = run_as(&cmd, &home, false);
    let (s, sr) = run_as(&cmd, &home, true);
    assert_eq!(
        m, "allow",
        "main must allow a literal-list loop writing inside its own dir; reason={mr}"
    );
    assert_eq!(
        s, "allow",
        "subagent must allow a literal-list loop writing inside its own dir; reason={sr}"
    );
}

/// Control: the word list is a command substitution, so `$f` is NOT
/// text-determined; the write really is undetermined and must stay not-allow.
#[test]
fn control_loop_over_command_substitution_is_not_allowed() {
    let home = unique_home("ls");
    let cmd = loop_cmd(&home, "$(ls)");
    let (m, mr) = run_as(&cmd, &home, false);
    assert_ne!(
        m, "allow",
        "an undetermined loop value must not be allowed; reason={mr}"
    );
}

/// Control: a symlink to a system directory created earlier on the line and then
/// written through must stay not-allow (the restriction is right where the
/// target really is a link).
#[test]
fn control_write_through_earlier_symlink_is_not_allowed() {
    let home = unique_home("ln");
    let d = home.0.join("wt").join("run");
    let d = d.display();
    let cmd =
        format!("D={d}; ln -s /etc $D/lnk; echo x > $D/lnk/a.tmp && mv $D/lnk/a.tmp $D/lnk/a.json");
    let (m, mr) = run_as(&cmd, &home, false);
    assert_ne!(
        m, "allow",
        "a write through a symlink into /etc must not be allowed; reason={mr}"
    );
}

// ---------------------------------------------------------------------------
// Controls added 2026-11-19 (condukt run-20261119-072406-79324 t2, retry): a
// RELATIVE symlink source spelled through a literal-list loop variable.
// `ln -s etc L` makes `L` point at the TEXT `etc`, resolved from the directory
// holding `L` — not from the cwd. 21d8ea853 replaced the source word by the
// value the AST evaluator proved, which is already placed against the cwd
// (`HOME/wt/a/b/c/d/etc`), so the write through the link was judged as landing
// in the tree.
//
// Observed on 21d8ea853 (this file's harness, temporary diagnostic test, cwd =
// HOME/wt, HOME under /tmp):
//   cd D && for f in etc; do ln -s $f /private/L; done; echo x > /private/L/passwd
//       -> allow (no output)
//   cd D && ln -s etc /private/L; echo x > /private/L/passwd
//       -> deny 'redirect /private/etc/passwd' writes into a system directory
//   cd D && for f in etc; do ln -s ../$f /tmp/L; done; echo x > /tmp/L/passwd
//       -> ask (not modelled ... into `/tmp/L`) in this harness; the main
//          session reported allow for the same shape with its own probe (cwd =
//          scratchpad/home/wt), not reproduced here.
//   cd D && for f in etc; do cp -s $f /private/L; done; ...
//       -> ask (`cp` option `-s` not modelled, another layer)
//   cd D && for f in etc tmp; do ln -s $f /private/L; done; ...
//       -> ask (more than one command of the line relinks this path)
//
// Asserted: every loop form != "allow"; the no-loop reference forms == "deny".
// DOES NOT PROVE: that the loop forms are Deny (only != allow: a symlink source
// spelled through a loop variable stays an Ask), why the `../$f` form is an
// Ask here but was reported allow by the main session, anything about absolute
// symlink sources, or the subagent verdict.
// ---------------------------------------------------------------------------

/// A scratch cwd four levels below HOME/wt and a link name that does not exist.
fn deep_cwd_and_name(home: &Home, tag: &str) -> (String, String) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let deep = home.0.join("wt").join("a").join("b").join("c").join("d");
    std::fs::create_dir_all(&deep).unwrap();
    let name = format!(
        "bg-loopln-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    );
    (deep.display().to_string(), name)
}

/// Reference: no loop; `etc` from `/private` is `/private/etc` -> system harm.
#[test]
fn control_no_loop_relative_symlink_source_into_etc_is_denied() {
    let home = unique_home("ln-noloop");
    let (deep, name) = deep_cwd_and_name(&home, "noloop");
    let cmd = format!("cd {deep} && ln -s etc /private/{name}; echo x > /private/{name}/passwd");
    let (m, mr) = run_as(&cmd, &home, false);
    assert_eq!(
        m, "deny",
        "a write through `ln -s etc /private/L` lands in /private/etc; reason={mr}"
    );
}

/// The same line with the source spelled through a literal-list loop
/// variable lands in `/private/etc` exactly as above: never allow.
#[test]
fn control_loop_relative_symlink_source_into_etc_is_not_allowed() {
    let home = unique_home("ln-loop");
    let (deep, name) = deep_cwd_and_name(&home, "loop");
    let cmd = format!(
        "cd {deep} && for f in etc; do ln -s $f /private/{name}; done; echo x > /private/{name}/passwd"
    );
    let (m, mr) = run_as(&cmd, &home, false);
    assert_ne!(
        m, "allow",
        "`ln -s $f /private/L` with f=etc lands in /private/etc; reason={mr}"
    );
}

/// The shape the main session reported: `../$f` from a link in `/tmp`.
#[test]
fn control_loop_dotdot_symlink_source_into_etc_is_not_allowed() {
    let home = unique_home("ln-dotdot");
    let (deep, name) = deep_cwd_and_name(&home, "dotdot");
    let cmd = format!(
        "cd {deep} && for f in etc; do ln -s ../$f /tmp/{name}; done; echo x > /tmp/{name}/passwd"
    );
    let (m, mr) = run_as(&cmd, &home, false);
    assert_ne!(
        m, "allow",
        "`ln -s ../$f /tmp/L` with f=etc lands in /etc; reason={mr}"
    );
}

/// `cp -s` makes a symlink to the source text too.
#[test]
fn control_loop_relative_cp_symbolic_source_into_etc_is_not_allowed() {
    let home = unique_home("cps-loop");
    let (deep, name) = deep_cwd_and_name(&home, "cps");
    let reference =
        format!("cd {deep} && cp -s etc /private/{name}; echo x > /private/{name}/passwd");
    let (m, mr) = run_as(&reference, &home, false);
    assert_eq!(
        m, "deny",
        "a write through `cp -s etc /private/L` lands in /private/etc; reason={mr}"
    );
    let cmd = format!(
        "cd {deep} && for f in etc; do cp -s $f /private/{name}; done; echo x > /private/{name}/passwd"
    );
    let (m, mr) = run_as(&cmd, &home, false);
    assert_ne!(
        m, "allow",
        "`cp -s $f /private/L` with f=etc lands in /private/etc; reason={mr}"
    );
}

/// Several values for the symlink source: the link may point at any of them.
#[test]
fn control_loop_relative_symlink_source_several_values_is_not_allowed() {
    let home = unique_home("ln-multi");
    let (deep, name) = deep_cwd_and_name(&home, "multi");
    let onto = format!(
        "cd {deep} && for f in etc tmp; do ln -s $f /private/{name}; done; echo x > /private/{name}/passwd"
    );
    let (m, mr) = run_as(&onto, &home, false);
    assert_ne!(
        m, "allow",
        "`ln -s $f /private/L` with f in etc tmp may land in /private/etc; reason={mr}"
    );
    let dotdot = format!(
        "cd {deep} && for f in etc tmp; do ln -s ../$f /tmp/{name}; done; echo x > /tmp/{name}/passwd"
    );
    let (m, mr) = run_as(&dotdot, &home, false);
    assert_ne!(
        m, "allow",
        "`ln -s ../$f /tmp/L` with f in etc tmp may land in /etc; reason={mr}"
    );
}
