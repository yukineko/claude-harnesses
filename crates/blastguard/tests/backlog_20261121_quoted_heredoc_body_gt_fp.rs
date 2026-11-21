#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false positive, observed 2026-11-21, main 79efa2c7c era, blastguard
//! 0.2.857 source / 0.2.840 deployed): from the main session,
//! `cd <worktree> && cat >> docs/specs/blastguard.md <<'EOF' ... EOF` followed by
//! `git add ... && git commit -qm "..."` was blocked by PreToolUse. The body was
//! markdown containing a backticked path such as
//! `~/.backlog/drivers/<fnv1a64(canonical main tree) %016x>/<fnv1a64(session_id) %016x>.driver`.
//! Verbatim reason: "'> .driver`' destroys the file's current contents and
//! `.driver`` contains a shell expansion, so blastguard cannot tell which file
//! this names, let alone whether its contents are recoverable".
//!
//! The delimiter is QUOTED, so the body is literal data appended to a .md file
//! in the own worktree; the angle bracket inside the body text is not a redirect.
//!
//! Two readings: (a) real defect (false positive): the body of a quoted heredoc
//! is inert data (no expansion, no redirect) and blastguard re-lexes it as shell,
//! treating `> .driver`` as a truncating redirect; (b) intended: body lines that
//! look like shell are deliberately judged even for quoted delimiters. Tell
//! apart: the same body fed to `cat >> x.md` with the quoted delimiter is data
//! for `cat`, which never interprets it; if a body without the `>` + backtick
//! sequence is allow while this one is not, the verdict depends on body text
//! (reading a), not on what executes. The control shows a real truncating
//! redirect with an expansion target is still not allow.
//!
//! Measured 2026-11-21 on 0.2.858: the heredoc append alone (body = the path
//! line) is allow; adding EITHER a `cd` prefix OR a trailing
//! `git add && git commit` line makes it Deny, so the body text is judged as
//! shell once a second command exists in the line.
//!
//! RE-ANCHORED 2026-11-22 (user ruling, frozen-assert re-anchor): the cd prefix
//! in the two cd tests was `cd "$PWD"`. The reported shape is
//! `cd <worktree> && cat >> docs/specs/blastguard.md <<'EOF' ...` with a LITERAL
//! path, so the prefix is now `cd <absolute literal wt dir> && `, built at run
//! time. `$PWD` remains statically undetermined (a relative redirect after it
//! is Ask) and the rule is NOT loosened.
//!
//! Asserted: the original shape, cd-prefix + append, and append + trailing git
//! commit are all "allow" in main. Control: a real
//! `echo x > "$d/.driver"` truncation with an expansion target is != "allow".
//!
//! DOES NOT PROVE: which parser path produces the Deny, that other body
//! contents (e.g. `$(`, column-0 backticks) are handled, or that the original
//! multi-line markdown is allowed beyond the shapes encoded here.
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
        "bg-gtfp-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt/docs/specs")).unwrap();
    Home(dir)
}

fn run_main(command: &str, home: &Home) -> (String, String) {
    let p = serde_json::json!({
        "session_id": "gt-fp-20261121",
        "cwd": home.0.join("wt"),
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    });
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

/// Original shape: cd && cat >> md <<'EOF' with the backticked driver path, then
/// git add && git commit. The command is prefixed at run time with
/// `cd <literal wt dir> && ` (see `with_literal_cd`).
const ORIGINAL: &str = "cat >> docs/specs/blastguard.md <<'EOF'\n\
## Driver file\n\
\n\
The marker lives at `~/.backlog/drivers/<fnv1a64(canonical main tree) %016x>/<fnv1a64(session_id) %016x>.driver` per session.\n\
EOF\n\
git add docs/specs/blastguard.md && git commit -qm \"specs: driver file\"";

/// cd prefix + quoted-heredoc append, body = the path line only (no git).
/// and is preceded by an angle bracket + space.
const MINIMAL: &str = "cat >> docs/specs/blastguard.md <<'EOF'\n`~/.backlog/drivers/<fnv1a64(canonical main tree) %016x>/<fnv1a64(session_id) %016x>.driver`\nEOF";

/// No cd prefix, but a trailing git add/commit line after the heredoc.
const MINIMAL_GT: &str = "cat >> docs/specs/blastguard.md <<'EOF'\n`~/.backlog/drivers/<fnv1a64(canonical main tree) %016x>/<fnv1a64(session_id) %016x>.driver`\nEOF\ngit add docs/specs/blastguard.md && git commit -qm \"specs: driver\"";

/// Prefix `body` with `cd <absolute literal path of the test's own wt dir> && `.
/// The path is literal text in the command (the reported shape), never `$PWD`.
fn with_literal_cd(body: &str, home: &Home) -> String {
    format!("cd {} && {}", home.0.join("wt").display(), body)
}

#[test]
fn original_shape_is_allowed() {
    let home = unique_home("orig");
    let (d, r) = run_main(&with_literal_cd(ORIGINAL, &home), &home);
    assert_eq!(d, "allow", "original shape must be allow; reason={r}");
}

#[test]
fn cd_prefix_then_heredoc_append_is_allowed() {
    let home = unique_home("min");
    let (d, r) = run_main(&with_literal_cd(MINIMAL, &home), &home);
    assert_eq!(
        d, "allow",
        "cd-prefix + heredoc append must be allow; reason={r}"
    );
}

#[test]
fn heredoc_append_then_git_commit_is_allowed() {
    let home = unique_home("mingt");
    let (d, r) = run_main(MINIMAL_GT, &home);
    assert_eq!(
        d, "allow",
        "heredoc append + trailing git commit must be allow; reason={r}"
    );
}

/// Control: a real truncating redirect whose target has an expansion is still
/// not allow (this one passes today and must keep passing).
#[test]
fn control_real_truncation_with_expansion_target_is_not_allow() {
    let home = unique_home("ctl");
    let (d, r) = run_main("echo x > \"$d/.driver\"", &home);
    assert_ne!(
        d, "allow",
        "real truncation w/ expansion must not be allow; reason={r}"
    );
}
