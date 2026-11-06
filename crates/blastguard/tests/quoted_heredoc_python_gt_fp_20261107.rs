// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro of a blastguard false positive observed 2026-11-07 in a subagent
//! session (agent a4265a5c8acbb7f5e, session 0d3d5bb8): a harmless
//! `python3 - "$F" <<'PY' … PY` regex rewrite was DENIED with
//!
//! ```text
//! `python3 - "$F" <<'PY'` takes its program from stdin, and the program text
//! contains a `>` that a shell would read as a redirect whose target cannot be
//! determined ('> ' destroys the file's current contents and `` is a relative
//! path and blastguard does not know which directory it is relative to, so it
//! cannot tell whether these bytes are recoverable); in program text it may be
//! an operator or part of a string, but blastguard cannot show that it is not
//! a redirect. Recommended: do not run this.
//! ```
//!
//! MEASURED (this file's probes, 2026-11-07 at d9863f70): the heredoc body of
//! that command contains NO `>` at all. The `>` that decides the verdict is
//! the single-quoted grep pattern `'^>'` on the shell line AFTER the `PY`
//! terminator (`diff … | grep '^>' | wc -l`): dropping that trailing line
//! makes the command Allow, and the trailing line alone is Allow. The minimal
//! shape is `python3 - <<'PY'\nprint(1)\nPY\necho '>'` (Deny in a subagent),
//! while `echo 'a > b'` in the same position is Allow. A `>` used as a
//! comparison or inside a string in the quoted body (`if len(s) > 0:`,
//! `print('a > b')`) is ALREADY Allow — pinned below as regression guards.
//!
//! So the defect is attribution: a quoted, non-redirect `>` in a LATER shell
//! command is reported as "the program text contains a `>`" of the heredoc
//! program. The program the heredoc feeds writes only `open(p,'w')` to the
//! argv path `$F`; the same command without the trailing line is Allow, so
//! nothing else in it is what decides the Deny.
//!
//! Contract (crates/blastguard/src/detect/interpreter.rs,
//! `interpreter_code_verdict`, user ruling R2 2026-10-14): "The shell pass
//! over the program text reads every `>` as a redirect, so `print(1 > 0)` or
//! `s = 'x > \`y\`'` used to be DENIED … When that is the ONLY Deny the
//! program text produces, the verdict is an Ask carrying [`RECOMMEND_NO`]".
//! That Ask is about the PROGRAM TEXT; the `>` here is not program text.
//!
//! Targets assert: the verdict equals the verdict of the same command with
//! that `>` replaced by a neutral `X`, and the reason is not the program-text
//! `>` finding. Controls pin what a fix must keep denying.
//!
//! Payloads carry `agent_id` so blastguard judges as in a subagent (the
//! observed context), where an Ask is hardened to Deny.
//! Written by a disinterested author (CLAUDE.md §2(a)); `src/` untouched.

#[path = "support/bounded.rs"]
mod bounded;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The program-text `>` finding (interpreter.rs `interpreter_code_verdict`).
const GT_REASON: &str = "the program text contains a `>` that a shell would read as a redirect";

fn fresh_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "blastguard-qheredoc-gt-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

/// (decision, reason) for `command` judged as a subagent Bash call.
/// Empty stdout is Allow.
fn verdict(cwd: &Path, command: &str) -> (String, String) {
    let bin = env!("CARGO_BIN_EXE_blastguard");
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "cwd": cwd.to_string_lossy(),
        "agent_id": "test-subagent",
        "tool_input": { "command": command },
    })
    .to_string();
    let mut hook = Command::new(bin);
    hook.current_dir(cwd)
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env("CLAUDE_PROJECT_DIR", cwd)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("BLASTGUARD_ASK")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut hook, payload.as_bytes(), bounded::HOOK_DEADLINE);
    assert_eq!(
        out.status.code(),
        Some(0),
        "hook must exit 0 (cmd: {command:?}); stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return ("allow".to_string(), String::new());
    }
    let doc: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout is not JSON for {command:?} ({e}): {stdout}"));
    let hso = doc.get("hookSpecificOutput");
    let decision = hso
        .and_then(|h| h.get("permissionDecision"))
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("no permissionDecision for {command:?}, got: {stdout}"))
        .to_string();
    let reason = hso
        .and_then(|h| h.get("permissionDecisionReason"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    (decision, reason)
}

/// The exact command from the subagent log (extracted with jq from the
/// tool_use input, toolu_01KFn28DmFq5Mr2mFXCPok5u).
const EXACT: &str = "cd /Users/yuki/src/.harness-worktrees/session-0d3d5bb8 && F=crates/blastguard/tests/git_exec_subcommand_failopen_20261107.rs; S=/private/tmp/claude-502/-Users-yuki-src-harness/0d3d5bb8-8523-4fec-984e-d95758ce5535/scratchpad; cp $F $S/committed.rs; python3 - \"$F\" <<'PY'
import re,sys
p=sys.argv[1]; s=open(p).read()
def add(reason):
    return lambda m: m.group(1)+f'#[ignore = \"backlog fb338793: {reason}\"]\\n'+m.group(2)
s=re.sub(r'(#\\[test\\]\\n)(fn ban_git_)', add(\"git exec-capable shape must be Deny (ban ruling)\"), s)
s=re.sub(r'(#\\[test\\]\\n)(fn (?:git_bisect|git_rebase|git_submodule|rm_)\\w*_is_deny)', add(\"system-harming command must be Deny\"), s)
s=re.sub(r'(#\\[test\\]\\n)(fn second_identical_)', add(\"repeated system-harming deny must stay Deny\"), s)
open(p,'w').write(s)
PY
grep -c 'ignore = \"backlog fb338793' $F; diff $S/committed.rs $F | grep '^>' | wc -l";

/// Assert `cmd` is judged exactly like `neutral` (the same command with the
/// non-redirect `>` replaced by `X`) and not on the program-text `>` reason.
#[track_caller]
fn assert_judged_like_neutral(tag: &str, cmd: &str, neutral: &str) {
    assert_ne!(cmd, neutral, "[{tag}] neutral variant must differ");
    let base = fresh_dir(tag);
    let (d, r) = verdict(&base, cmd);
    let (nd, nr) = verdict(&base, neutral);
    assert!(
        !r.contains(GT_REASON) && d == nd && r == nr,
        "[{tag}] a quoted `>` outside the heredoc body was judged as heredoc program text:\n\
         got {d} (reason: {r})\nneutral variant: {nd} (reason: {nr})\ncmd: {cmd:?}"
    );
}

// ---------------------------------------------------------------- targets

/// TARGET (RED at d9863f70): the exact observed command.
#[test]
#[ignore = "backlog PENDING: quoted `>` after a python stdin heredoc is judged as its program text"]
fn exact_observed_python_heredoc_command_is_not_denied_for_a_later_quoted_gt() {
    let neutral = EXACT.replace("grep '^>'", "grep '^X'");
    assert_judged_like_neutral("exact", EXACT, &neutral);
}

/// TARGET (RED at d9863f70): minimal shape — a program that writes nothing,
/// then a shell command whose single-quoted argument is `>`.
#[test]
#[ignore = "backlog PENDING: quoted `>` after a python stdin heredoc is judged as its program text"]
fn minimal_python_heredoc_then_quoted_gt_argument_is_not_denied() {
    for (i, (cmd, neutral)) in [
        (
            "python3 - <<'PY'\nprint(1)\nPY\necho '>'",
            "python3 - <<'PY'\nprint(1)\nPY\necho 'X'",
        ),
        (
            "python3 - <<'PY'\nprint(1)\nPY\ngrep '^>' x",
            "python3 - <<'PY'\nprint(1)\nPY\ngrep '^X' x",
        ),
        (
            "python3 - <<'PY'\nprint(1)\nPY\ngrep \"^>\" x",
            "python3 - <<'PY'\nprint(1)\nPY\ngrep \"^X\" x",
        ),
    ]
    .iter()
    .enumerate()
    {
        assert_judged_like_neutral(&format!("min{i}"), cmd, neutral);
    }
}

// ------------------------------------- regression guards (pass at d9863f70)

/// A `>` used as a comparison / inside a string in a QUOTED heredoc python
/// body, with a program that writes nothing, is not denied for that `>`.
#[test]
fn quoted_heredoc_python_body_gt_comparison_or_string_is_not_denied() {
    for (i, cmd) in [
        "F=a.rs; python3 - \"$F\" <<'PY'\nimport sys\ns=open(sys.argv[1]).read()\nif len(s) > 0:\n    print('nonempty')\nPY",
        "python3 - <<'PY'\nprint('a > b')\nPY",
        "python3 - <<'PY'\nprint('a > b')\nPY\necho 'a > b'",
    ]
    .iter()
    .enumerate()
    {
        let base = fresh_dir(&format!("guard{i}"));
        let (d, r) = verdict(&base, cmd);
        assert!(
            d == "allow" && !r.contains(GT_REASON),
            "[guard{i}] got {d} (reason: {r}) for {cmd:?}"
        );
    }
}

// --------------------------------------------------------------- controls

/// CONTROL: a real shell redirect onto a protected path stays denied —
/// unquoted heredoc body fed to a shell, quoted heredoc body fed to a shell
/// (the shell DOES parse it), and a real redirect on the line after a python
/// stdin heredoc.
#[test]
fn control_real_redirect_to_protected_path_stays_denied() {
    for (i, cmd) in [
        "sh <<EOF\necho x > .git/hooks/pre-commit\nEOF",
        "bash <<EOF\necho x > .claude/settings.json\nEOF",
        "sh <<'EOF'\necho x > .git/hooks/pre-commit\nEOF",
        "python3 - <<'PY'\nprint(1)\nPY\necho x > .git/hooks/pre-commit",
    ]
    .iter()
    .enumerate()
    {
        let base = fresh_dir(&format!("ctl{i}"));
        let (d, r) = verdict(&base, cmd);
        assert_eq!(d, "deny", "[ctl{i}] {cmd:?} must stay denied; reason: {r}");
    }
}

/// CONTROL: `python3 -c` writing a protected path stays denied.
#[test]
fn control_python_c_writing_protected_path_stays_denied() {
    for (i, cmd) in [
        "python3 -c \"open('.git/hooks/pre-commit','w').write('x')\"",
        "python3 -c \"open('.claude/settings.json','w').write('x')\"",
    ]
    .iter()
    .enumerate()
    {
        let base = fresh_dir(&format!("ctlc{i}"));
        let (d, r) = verdict(&base, cmd);
        assert_eq!(d, "deny", "[ctlc{i}] {cmd:?} must stay denied; reason: {r}");
    }
}
