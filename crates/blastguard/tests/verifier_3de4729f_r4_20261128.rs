#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Independent adversarial verification, ROUND 4, of backlog 3de4729f
//! (branch `fix-3de4729f`, HEAD 6fdb22556 = blastguard 0.2.1047, base
//! cf33a5b2c). Written by a verifier, not the implementer (CLAUDE.md §2(a)).
//!
//! Target: `ast::Script.field_words` (the `Mode::Field` words) and
//! `relink::word_evaluates` / `zsh_runs_code`: an AST-listed word (or, when
//! the AST cannot list them, a blank-separated piece of the line) holding an
//! unquoted non-leading `(`, a leading `=(`, `$~` / `${~`, or `${(` makes
//! `relinked` return Err (Ask) and `line_is_relink_data` false.
//!
//! Execution observed 2026-11-28 with zsh 5.9 (`zsh -fc`) in the verifier's
//! scratch dir (a file `f1`, a dir `tgt`), each creating a link named below:
//! - `${(e):-\$(ln -s tgt X)}` runs in: a `case` subject (b9, b11), a
//!   `case` pattern (b10), a `[[ ]]` operand (b8), an assignment value (b12),
//!   `local` (b13), `$(( ))` (d1, d7), `$[ ]` (d8), a here-string (d3);
//!   `(ej: :)` (b2) and `"${(Le)…}"` (b3) run it too.
//! - `setopt promptsubst; print -P "\$(ln -s tgt b4)"` (b4); without
//!   `promptsubst` it printed the text (no b5).
//! - A glob qualifier after a command substitution whose QUOTED text holds
//!   a paren: `cd $(printf "" "(")*(e:"ln -s tgt e1":)` (e1),
//!   `cd $(printf "" ")")*(e:…)` (e2), `cat < $(printf "" "(")*(e:…)` (e3),
//!   and with a backtick substitution (e4). The substitution prints nothing,
//!   so the word is `*(e:…)`.
//! - A function body (d5) and a nested `$( )` (d6) run a qualifier.
//!
//! Observed NOT to execute: a glob qualifier in a `case` subject (a1), a
//! `case` pattern (a2), `$~X` as a `case` pattern (a3), `[[ -e f1(e:…) ]]` /
//! `[[ -n *(e:…) ]]` / `[[ f1 == f1(e:…) ]]` (b6, b7, a4); an `alias`
//! defined in the same `zsh -fc` string ("command not found: zz").
//! Not observed (the probe command was refused by blastguard in the
//! verifier's own session, relays bgr-9c5e0250deb2 / bgr-7a59454d2b72 /
//! bgr-23f667c68dba): `${(e)…}` in an unquoted here-document body, `eval`,
//! and `${(e)…}` as a redirect target.
//!
//! Measured 2026-11-28, base cf33a5b2c vs fix 6fdb22556 (this file copied to
//! a detached worktree of cf33a5b2c and run there):
//! - `FP_*` (RED, ignored): Allow at base, Ask at the fix with reason "the
//!   line holds a zsh construct that runs code while a word is expanded …".
//! - `FAILOPEN_SUBST_QUOTED_PAREN` (RED, ignored): Allow at base AND fix —
//!   pre-existing, but it bypasses this branch's guard: `skip_parens` counts
//!   parens inside quotes, so a quoted `(` / `)` inside `$( )` hides the
//!   qualifier that follows the substitution.
//! - `FAILOPEN_PRINT_P_PROMPTSUBST` (RED, ignored): Allow at base AND fix —
//!   pre-existing, outside the scope of `field_words`.
//! - `FP_PREEXISTING_UNMODELLED` (RED, ignored): Ask at base AND fix, with a
//!   reason other than this branch's — pre-existing.
//! - Controls (not Allow): the `case` subject, function body, nested `$( )`,
//!   backtick, `<( )`, quote-split fallback and bare `cd *(e:…)` shapes were
//!   Allow at base — this branch made them stricter.
//! - controls: verdicts identical in kind to the expectation at the fix.
//!
//! DOES NOT PROVE: subagent-mode verdicts; reason texts; that the shapes
//! below are the only ones; any zsh option set other than `-f` defaults
//! (plus the `setopt` written); how Claude Code's own shell wrapper treats
//! aliases.

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/fixed_tmp.rs"]
mod fixed_tmp;
#[path = "support/git_env.rs"]
mod git_env;
#[path = "support/neutral_base.rs"]
mod neutral_base;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Fx {
    base: PathBuf,
    scratch_root: PathBuf,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
        let _ = std::fs::remove_dir_all(&self.scratch_root);
    }
}

impl Fx {
    fn home(&self) -> PathBuf {
        self.base.join("home")
    }
    fn scratch(&self) -> PathBuf {
        self.scratch_root.join("scratchpad")
    }
    fn s(&self) -> String {
        self.scratch().to_string_lossy().into_owned()
    }
    fn repo(&self) -> PathBuf {
        self.base.join("src").join("proj")
    }
    fn wt(&self) -> PathBuf {
        self.base
            .join("src")
            .join(".harness-worktrees")
            .join("wt-own")
    }
    fn git(&self, dir: &Path, args: &[&str]) {
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
            .env("HOME", self.home());
        let out =
            bounded::run_bounded_stdin(&mut cmd, bounded::ChildStdin::Null, bounded::TOOL_DEADLINE);
        assert!(
            out.status.success(),
            "fixture git {args:?} in {} failed: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

fn fixture(tag: &str) -> Fx {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let base = neutral_base::neutral_base().join(format!("verifier-3de4729f-r4-{tag}-{n}"));
    let scratch_root = fixed_tmp::canonical_fixed_temp_root().join(format!(
        "bg-verifier-3de4729f-r4-{tag}-{}-{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::remove_dir_all(&scratch_root);
    for d in [
        base.join("home"),
        base.join("src").join("proj"),
        base.join("src").join(".harness-worktrees"),
        base.join("store"),
        base.join("tmpdir"),
        scratch_root.join("scratchpad"),
    ] {
        std::fs::create_dir_all(d).unwrap();
    }
    let fx = Fx { base, scratch_root };
    std::fs::write(fx.scratch().join("in.json"), "{}\n").unwrap();
    let repo = fx.repo();
    std::fs::write(repo.join("README"), "x\n").unwrap();
    fx.git(&repo, &["init", "-q", "-b", "main"]);
    fx.git(&repo, &["add", "-A"]);
    fx.git(&repo, &["commit", "-q", "-m", "init"]);
    let wt = fx.wt();
    fx.git(
        &repo,
        &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "own"],
    );
    std::fs::write(wt.join("in.json"), "{}\n").unwrap();
    std::fs::write(wt.join("f.rs"), "\n").unwrap();
    fx
}

fn run_bin(bin: &str, fx: &Fx, command: &str) -> (String, String) {
    let cwd = fx.wt();
    let p = serde_json::json!({
        "session_id": "verifier-3de4729f-r4-20261128",
        "cwd": cwd,
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    });
    let mut cmd = Command::new(bin);
    cmd.env_clear()
        .env("HOME", fx.home())
        .env("PATH", "/usr/bin:/bin")
        .env("TMPDIR", fx.base.join("tmpdir"))
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env("CLAUDE_PROJECT_DIR", &cwd)
        .env("BLASTGUARD_APPROVALS_DIR", fx.base.join("store"))
        .current_dir(&cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    git_env::scrub(&mut cmd);
    let out = bounded::run_bounded(&mut cmd, p.to_string().as_bytes(), bounded::HOOK_DEADLINE);
    assert!(
        out.status.success(),
        "hook exited {:?} for {command:?}: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return ("allow".into(), String::new());
    }
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("hook stdout for {command:?} is not JSON ({e}): {stdout:?}"));
    let h = &v["hookSpecificOutput"];
    (
        h["permissionDecision"].as_str().unwrap_or("?").to_string(),
        h["permissionDecisionReason"]
            .as_str()
            .unwrap_or("")
            .to_string(),
    )
}

fn run_main(fx: &Fx, command: &str) -> (String, String) {
    run_bin(env!("CARGO_BIN_EXE_blastguard"), fx, command)
}

/// Every command whose decision is NOT one of `want`, with decision and reason.
fn not_in(fx: &Fx, cmds: &[String], want: &[&str]) -> Vec<String> {
    cmds.iter()
        .filter_map(|c| {
            let (d, r) = run_main(fx, c);
            (!want.contains(&d.as_str())).then(|| format!("{c:?} => {d} ({r})"))
        })
        .collect()
}

fn expect(fx: &Fx, cmds: &[String], want: &[&str], why: &str) {
    let bad = not_in(fx, cmds, want);
    assert!(
        bad.is_empty(),
        "expected {want:?} ({why}):\n{}",
        bad.join("\n")
    );
}

fn owned(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

// ---------------------------------------------------------------- B. FP --

/// A POSIX function definition `name()` on the line: the AST does not list
/// the words, and the blank-separated piece `f()` holds a non-leading `(`.
/// No code runs while a word is expanded. Allow at base cf33a5b2c, Ask at
/// 6fdb22556 (measured 2026-11-28). `function f { … }` stays Allow (control).
const FP_FUNCTION_DEFINITION: &[&str] = &[
    "f() { echo hi; }; f > out.txt",
    "foo() { cargo test; }; foo > log.txt 2>&1",
    "run() { cargo test -p x; }; run 2>&1 | tail > out.txt",
    "f() { echo \"a b(c\"; }; f > out.txt",
    "cat > f.rs <<'EOF'\nfn f(x: u8) {}\nEOF\ng() { :; }; g > out.txt",
];

#[test]
fn fp_function_definition_is_allow() {
    let fx = fixture("fp-fn");
    expect(
        &fx,
        &owned(FP_FUNCTION_DEFINITION),
        &["allow"],
        "a function definition runs no code while a word is expanded",
    );
}

/// A `(` inside an unquoted `${…}` operand (bash default / pattern text):
/// no glob qualifier, no code. Allow at base, Ask at 6fdb22556.
const FP_PAREN_IN_PARAM_OPERAND: &[&str] = &["echo ${FOO:-(none)} > f", "echo ${var%%(*} > f"];

#[test]
fn fp_paren_in_param_operand_is_allow() {
    let fx = fixture("fp-param");
    expect(
        &fx,
        &owned(FP_PAREN_IN_PARAM_OPERAND),
        &["allow"],
        "a parameter operand paren runs no code",
    );
}

/// zsh qualifiers / bash extglob that run no code (`(.)`, `(N)`, `!(x)`):
/// over-approximated by design ("a glob qualifier
/// `(…)` of ANY spelling"). Allow at base, Ask at 6fdb22556.
const FP_NON_EXECUTING_PAREN_GLOB: &[&str] = &["ls *(.) > f", "ls **/*.rs(N) > f", "ls !(x) > f"];

#[test]
fn fp_non_executing_paren_glob_is_allow() {
    let fx = fixture("fp-glob");
    expect(
        &fx,
        &owned(FP_NON_EXECUTING_PAREN_GLOB),
        &["allow"],
        "no code runs",
    );
}

/// A quoted `)` inside `$( )` ends `skip_parens` early, exposing a quoted
/// `(`. Allow at base, Ask at 6fdb22556.
const FP_SUBST_QUOTED_PAREN: &[&str] = &["echo $(printf ')(') > f"];

#[test]
fn fp_subst_quoted_paren_is_allow() {
    let fx = fixture("fp-subst");
    expect(
        &fx,
        &owned(FP_SUBST_QUOTED_PAREN),
        &["allow"],
        "no code runs",
    );
}

/// Pre-existing FPs met on the way, NOT from this branch: Ask at base AND
/// 6fdb22556 with "… may create or rename an entry on this path and is not
/// modelled …" (measured 2026-11-28). No relinking command is on the line.
const FP_PREEXISTING_UNMODELLED: &[&str] = &[
    "echo $(( $(wc -l < in.json) + 1 )) > n",
    "echo ${arr[(r)x]} > f",
];

#[test]
#[ignore = "RED: FP (pre-existing, Ask at base and 6fdb22556) — arithmetic / subscript-flag word read as an unmodelled relinker"]
fn fp_preexisting_unmodelled_is_allow() {
    let fx = fixture("fp-pre");
    expect(
        &fx,
        &owned(FP_PREEXISTING_UNMODELLED),
        &["allow"],
        "no relinking command on the line",
    );
}

/// Everyday shapes: Allow at base AND 6fdb22556 (measured 2026-11-28).
const CONTROL_ALLOW: &[&str] = &[
    "cargo test -- --nocapture > log.txt",
    "git log --format='%h(%an)' > out.txt",
    "printf '%s(%d)\\n' a 1 > f",
    "echo \"f(x)\" > f",
    "grep -E 'a(b|c)' in.json > y",
    "find . -name '*.rs' > list",
    "awk '{print $1}' in.json > f",
    "ls *.rs > f",
    "jq '.a | map(.b)' in.json > f",
    "echo a\\(b > f",
    "echo foo\\(bar\\) > f",
    "function f { echo hi; }; f > out.txt",
    "f () { echo hi; }; f > out.txt",
    "cat > f.rs <<EOF\nfn f(x: u8) {}\nEOF\ncargo test > log.txt",
    "cat > f.rs <<'EOF'\nfn f(x: u8) {}\nEOF\ncargo test > log.txt",
    "cat > f.rs <<EOF\nfn f(x: u8) {}\nEOF\necho done > log.txt",
    "cat > a.txt <<EOF\nhello (world)\nEOF\nwc -l a.txt > n.txt",
    "cat > f.sh <<EOF\nmain() { echo hi; }\nEOF\necho ok > log.txt",
    "cat <<EOF > notes.md\n- fix(parser): x\nEOF\ngit add notes.md",
    "git log --format='%h (%an)' -- '*.rs' > f",
    "cargo test 2>&1 | grep -E 'test result: (ok|FAILED)' > f",
    "echo \"$(date)\" > f",
    "echo $((1+(2*3))) > f",
    "diff <(sort a) <(sort b) > d",
    "printf '%s\\n' \"${arr[@]}\" > f",
    "printf '%s\\n' $(seq 1 3) > f",
    "awk 'BEGIN{print(1)}' > f",
    "python3 -c 'print((1,2))' > f",
    "sed -E 's/(a)/\\1/' in.json > y",
    "for f in *.rs; do echo $f; done > list",
    "while read l; do echo \"$l\"; done < in.json > out",
    "make -j$(nproc) > log",
    "echo ${#arr[@]} > f",
    "rg 'fn \\w+\\(' > f",
    "test -f x && echo \"(ok)\" > f",
    "[[ -f x ]] && echo ok > f",
    "printf $'a(b)\\n' > f",
    "echo \"${x//(/[}\" > f",
    "x=$(git rev-parse HEAD); echo \"$x\" > f",
    "echo $(git log -1 --format='%s)') > f",
    "cargo build 2>&1 | tee build.log",
    "echo hi > out.txt # see foo(bar)",
    "echo 'a (b' \"c d(e\" > f",
];

#[test]
fn control_everyday_shapes_stay_allow() {
    let fx = fixture("ctl-allow");
    expect(
        &fx,
        &owned(CONTROL_ALLOW),
        &["allow"],
        "Allow at base and fix",
    );
}

// --------------------------------------------------------- A. fail-open --

/// A glob qualifier glued after a `$( )` whose QUOTED text holds `(` or `)`
/// (observed executing: e1, e2, e3). `skip_parens` is not quote-aware, so
/// `word_evaluates` either skips to the end of the word or resumes with a
/// flipped double-quote state. PRE-EXISTING: Allow at base cf33a5b2c AND at
/// 6fdb22556 (measured 2026-11-28) — but a bypass of this branch's guard,
/// which Asks the same line without the substitution (control below).
fn failopen_subst_quoted_paren(s: &str) -> Vec<String> {
    vec![
        format!("cd $(printf \"\" \"(\")*(e:'ln -s /etc {s}/d':); echo x > {s}/d/hosts"),
        format!("cd $(printf \"\" \")\")*(e:'ln -s /etc {s}/d':); echo x > {s}/d/hosts"),
        format!("cd \"$(printf \"\" \"(\")\"*(e:'ln -s /etc {s}/d':); echo x > {s}/d/hosts"),
        format!("cd $(echo '(')*(e:'ln -s /etc {s}/d':); echo x > {s}/d/hosts"),
        format!("cat < $(printf \"\" \"(\")*(e:'ln -s /etc {s}/d':); echo x > {s}/d/hosts"),
        format!("< $(printf \"\" \"(\")*(e:'ln -s /etc {s}/d':); echo x > {s}/d/hosts"),
        format!("echo $(printf \"\" \"(\")*(e:'ln -s /etc {s}/d':); echo x > {s}/d/hosts"),
        format!("cd $(printf \"\" \"(\")*(e:'ln -s /etc {s}/d':) && echo x > {s}/d/hosts"),
        format!(
            "cd $(printf \"\" \"(\")*(e:'ln -s ~/.claude {s}/d':); echo x > {s}/d/settings.json"
        ),
        format!("cd $(printf \"\" \"(\")*(e:'ln -s /etc {s}/d':); cp {s}/in.json {s}/d/hosts"),
    ]
}

#[test]
fn failopen_subst_quoted_paren_stays_not_allow() {
    let fx = fixture("fo-subst");
    expect(
        &fx,
        &failopen_subst_quoted_paren(&fx.s()),
        &["ask", "deny"],
        "qualifier after a substitution runs ln",
    );
}

/// `setopt promptsubst; print -P '$(ln …)'` (observed executing: b4).
/// PRE-EXISTING: Allow at base AND 6fdb22556 (measured 2026-11-28).
#[test]
fn failopen_print_p_promptsubst_stays_not_allow() {
    let fx = fixture("fo-printp");
    let s = fx.s();
    let cmds = vec![format!(
        "setopt promptsubst; print -P '$(ln -s /etc {s}/d)'; echo x > {s}/d/hosts"
    )];
    expect(&fx, &cmds, &["ask", "deny"], "print -P runs ln");
}

/// Controls: executing shapes NOT Allow at 6fdb22556 (measured 2026-11-28).
/// `case` subject and the nested / function / backtick / `<( )` / quote-split
/// shapes were Allow at base (this branch made them stricter).
fn control_not_allow(s: &str) -> Vec<String> {
    vec![
        format!("case ${{(e):-\\$(ln -s /etc {s}/d)}} in *) ;; esac; echo x > {s}/d/hosts"),
        format!("case ${{(e):-\\$(ln -s /etc {s}/d)}} in *) echo x > {s}/d/hosts ;; esac"),
        format!("case x in ${{(e):-\\$(ln -s /etc {s}/d)}}) ;; esac; echo x > {s}/d/hosts"),
        format!("[[ ${{(e):-\\$(ln -s /etc {s}/d)}} == x ]]; echo x > {s}/d/hosts"),
        format!("[[ ${{(e):-\\$(ln -s /etc {s}/d)}} == x ]] && echo x > {s}/d/hosts"),
        format!("echo $(( ${{(e):-\\$(ln -s /etc {s}/d)}} 0 )); echo x > {s}/d/hosts"),
        format!("echo \"$(( ${{(e):-\\$(ln -s /etc {s}/d)}} 0 ))\"; echo x > {s}/d/hosts"),
        format!("echo $[ ${{(e):-\\$(ln -s /etc {s}/d)}} 0 ]; echo x > {s}/d/hosts"),
        format!("echo $(( ${{(e):-\\$(ln -s /etc {s}/d)}} 0 )) > {s}/d/hosts"),
        format!("cat <<< ${{(e):-\\$(ln -s /etc {s}/d)}}; echo x > {s}/d/hosts"),
        format!("grep x <<< ${{(e):-\\$(ln -s /etc {s}/d)}}; echo x > {s}/d/hosts"),
        format!("cat <<EOF\n${{(e):-\\$(ln -s /etc {s}/d)}}\nEOF\necho x > {s}/d/hosts"),
        format!("cd ${{(e):-\\$(ln -s /etc {s}/d)}}; echo x > {s}/d/hosts"),
        format!("echo ${{(e):-\\$(ln -s /etc {s}/d)}}; echo x > {s}/d/hosts"),
        format!("echo ${{(ej: :):-\\$(ln -s /etc {s}/d)}}; echo x > {s}/d/hosts"),
        format!("echo \"${{(Le):-\\$(ln -s /etc {s}/d)}}\"; echo x > {s}/d/hosts"),
        format!("g() {{ echo *(e:'ln -s /etc {s}/d':); }}; g; echo x > {s}/d/hosts"),
        format!("echo $(echo *(e:'ln -s /etc {s}/d':)); echo x > {s}/d/hosts"),
        format!("echo \"$(echo *(e:'ln -s /etc {s}/d':))\"; echo x > {s}/d/hosts"),
        format!("echo `echo *(e:'ln -s /etc {s}/d':)`; echo x > {s}/d/hosts"),
        format!("cat <(echo *(e:'ln -s /etc {s}/d':)); echo x > {s}/d/hosts"),
        format!("echo \"a b\"*(e:'ln -s /etc {s}/d':); echo x > {s}/d/hosts"),
        format!("echo 'a b'*(e:'ln -s /etc {s}/d':); echo x > {s}/d/hosts"),
        format!("echo \"a b\"*(e:'l''n -s /etc {s}/d':); echo x > {s}/d/hosts"),
        format!("echo *(e:'l''n -s /etc {s}/d':); echo x > {s}/d/hosts"),
        format!("cd `printf \"\" \")\"`*(e:'ln -s /etc {s}/d':); echo x > {s}/d/hosts"),
        format!("cd *(e:'ln -s /etc {s}/d':); echo x > {s}/d/hosts"),
    ]
}

#[test]
fn control_executing_shapes_stay_not_allow() {
    let fx = fixture("ctl-not");
    expect(
        &fx,
        &control_not_allow(&fx.s()),
        &["ask", "deny"],
        "zsh runs ln",
    );
}

/// System target through the substitution bypass: Deny at base and fix.
#[test]
fn control_subst_bypass_system_target_stays_deny() {
    let fx = fixture("ctl-sys");
    let cmds = vec!["cd $(printf \"\" \"(\")*(e:'ln -s /etc d':); echo x > /etc/hosts".to_string()];
    expect(&fx, &cmds, &["deny"], "system harm");
}
