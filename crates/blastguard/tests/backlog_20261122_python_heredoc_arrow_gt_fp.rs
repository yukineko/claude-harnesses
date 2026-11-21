#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Backlog RED (2026-11-22, measured on main 81fbc18a5; observed by the t4
//! worker of condukt run run-20261121-225214-62742, relay bgr-ca5962ec07a4).
//!
//! The worker's command (shape kept, paths replaced by the fixture):
//! `cd <literal dir> && grep -n ... 2>/dev/null | head; python3 - <<'EOF'`
//! whose Python program replaces Rust source text with
//! `s.replace('''...''', '''...Some("eval") => return true,...''')` and
//! writes the file back with `open(p,'w')`. It was refused in the subagent
//! with: "'> return' is a RELATIVE redirect after an earlier cd/pushd/popd
//! whose target directory blastguard could not statically resolve". The `>`
//! is the second character of a Rust `=>` inside a Python string literal in a
//! QUOTED heredoc fed to `python3 -`; it is not a shell redirect, and the cd
//! target is a literal, existing directory.
//!
//! RED (`verbatim_*`, #[ignore]): the worker's command verbatim, only the cd
//! target replaced by an existing fixture dir; the reason must never quote
//! `> return` as a redirect. Measured 2026-11-22: main => ask, subagent =>
//! deny, both quoting "'> return' is a RELATIVE redirect after an earlier
//! cd/pushd/popd whose target directory blastguard could not statically
//! resolve".
//!
//! Control (`python_heredoc_arrow_text_is_not_a_redirect`, runs normally): a
//! REDUCED program (one `s.replace` with `=>` text after a literal cd) does
//! NOT reproduce — the decision equals the `|` twin and the reason does not
//! quote `> return`. So the trigger is something else in the verbatim command
//! (candidates: the glob operands `ast/*.rs ast.rs`, the backslash-escaped
//! `\|` in the grep pattern, or the second `s.replace` with a docstring). Not
//! isolated here.
//!
//! Readings: (a) FP: quoted-heredoc python string text is read as a shell
//! redirect (memory heredoc-body-by-target-language: .py body is python);
//! (b) intended: none found — the cd target is a literal existing dir.
//! Related: backlog a2d36aa2 (same reason text, but its RED needs an
//! UNRESOLVABLE cd and a `cat >>` body).
//!
//! DOES NOT PROVE: which part of the verbatim command triggers it; which
//! verdict the command should get (only that `> return` is not a redirect).

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/git_env.rs"]
mod git_env;

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
        "bg-pyarrow-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    Home(dir)
}

fn run_as(command: &str, cwd: &std::path::Path, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "pyarrow-20261122",
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

fn fixture_git(dir: &std::path::Path, home: &Home, args: &[&str]) {
    let mut cmd = git_env::pinned(dir);
    cmd.args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", &home.0);
    let out =
        bounded::run_bounded_stdin(&mut cmd, bounded::ChildStdin::Null, bounded::TOOL_DEADLINE);
    assert!(
        out.status.success(),
        "fixture git {args:?} in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `<home>/wt/proj` is a git work tree with `src/foo.rs` committed.
fn setup(tag: &str) -> (Home, std::path::PathBuf, std::path::PathBuf) {
    let home = unique_home(tag);
    let cwd = home.0.join("wt");
    let proj = cwd.join("proj");
    std::fs::create_dir_all(proj.join("src")).unwrap();
    std::fs::write(proj.join("src/foo.rs"), "// MARK\nfn main() {}\n").unwrap();
    fixture_git(&proj, &home, &["init", "-q", "-b", "main"]);
    fixture_git(&proj, &home, &["add", "src/foo.rs"]);
    fixture_git(&proj, &home, &["commit", "-q", "-m", "init"]);
    (home, cwd, proj)
}

fn cmd(proj: &std::path::Path, arm: &str) -> String {
    format!(
        "cd {} && grep -n \"fn main\" src/foo.rs 2>/dev/null | head; python3 - <<'EOF'\n\
p='src/foo.rs'\n\
s=open(p).read()\n\
s=s.replace('''// MARK''','''    match lit(0) {{\n\
        Some(\"eval\") {arm} return true,\n\
        _ {arm} return false,\n\
    }}\n\
// MARK''')\n\
open(p,'w').write(s)\n\
EOF\n\
sed -n 1,5p src/foo.rs",
        proj.display()
    )
}

#[test]
fn python_heredoc_arrow_text_is_not_a_redirect() {
    let mut bad = Vec::new();
    for subagent in [false, true] {
        let (home, cwd, proj) = setup(if subagent { "s" } else { "m" });
        let (td, tr) = run_as(&cmd(&proj, "|"), &cwd, &home, subagent);
        let (d, r) = run_as(&cmd(&proj, "=>"), &cwd, &home, subagent);
        if d != td || r.contains("> return") {
            bad.push(format!(
                "subagent={subagent}: `=>` text gave {d} | {r}\n  twin (`|`) gave {td} | {tr}"
            ));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// The worker command verbatim (bgr-ca5962ec07a4), only the cd target replaced.
const VERBATIM: &str = r#"cd {DIR} && grep -n "pub struct Word\|pub use.*Word" ast/*.rs ast.rs 2>/dev/null | head; python3 - <<'EOF'
p='payload/determined_output.rs'
s=open(p).read()
s=s.replace('''    let mut i = 0;
    match lit(0) {
        Some("eval") => return true,
        Some("builtin") => {
            i = 1;
            if lit(i) == Some("--") {
                i += 1;
            }
        }
        Some("command") => {
            i = 1;
            while lit(i) == Some("-p") {
                i += 1;
            }
            if lit(i) == Some("--") {
                i += 1;
            }
        }
        _ => return false,
    }
    lit(i) == Some("eval")''','''    let mut i = match lit(0) {
        Some("eval") => return true,
        Some("builtin" | "command") => 1,
        _ => return false,
    };
    if lit(0) == Some("command") {
        while lit(i) == Some("-p") {
            i += 1;
        }
    }
    if lit(i) == Some("--") {
        i += 1;
    }
    lit(i) == Some("eval")''')
s=s.replace('''/// [`resolution_fixed`], except that AT MOST ONE command on `cmd` may be a
/// bare `eval` (backlog d766b5fe).''','''/// [`resolution_fixed`], except that AT MOST ONE command on `cmd` may be an
/// `eval` — bare, or `command [-p] [--] eval` / `builtin [--] eval`
/// ([`runs_eval`], backlogs d766b5fe / bf40851f).''')
open(p,'w').write(s)
EOF
sed -n 92,135p payload/determined_output.rs"#;

fn verbatim_setup(tag: &str) -> (Home, std::path::PathBuf, std::path::PathBuf) {
    let (home, cwd, proj) = setup(tag);
    let dir = proj.join("src/detect");
    std::fs::create_dir_all(dir.join("payload")).unwrap();
    std::fs::create_dir_all(dir.join("ast")).unwrap();
    std::fs::write(dir.join("payload/determined_output.rs"), "fn f() {}\n").unwrap();
    std::fs::write(dir.join("ast.rs"), "pub struct Word;\n").unwrap();
    (home, cwd, dir)
}

#[test]
#[ignore = "RED: verbatim worker command (python heredoc with => return text) refused as a relative redirect"]
fn verbatim_worker_command_reason_does_not_quote_python_text_as_redirect() {
    let mut bad = Vec::new();
    for subagent in [false, true] {
        let (home, cwd, dir) = verbatim_setup(if subagent { "vs" } else { "vm" });
        let c = VERBATIM.replace("{DIR}", &dir.display().to_string());
        let (d, r) = run_as(&c, &cwd, &home, subagent);
        if r.contains("> return") {
            bad.push(format!("subagent={subagent}: {d} | {r}"));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}
