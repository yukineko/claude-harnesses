#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Independent adversarial verification, ROUND 3, of backlog 3de4729f
//! (branch `fix-3de4729f`, HEAD 0cda717c2 = blastguard 0.2.1045, base
//! cf33a5b2c). Written by a verifier, not the implementer (CLAUDE.md §2(a)).
//!
//! Target: `zsh_runs_code` (relink.rs): outside quotes a `(e` / `(+` glued
//! to a word, or `=(`; outside single quotes `${(…e…)…}`. When it holds,
//! `relinked` returns Err (Ask) and `line_is_relink_data` / `word_is_inert`
//! stop reading the line as data.
//!
//! Execution observed 2026-11-28 in the verifier's scratch dir with zsh 5.9
//! (`zsh -fc`), each creating `<name> -> ../tgt` with a file `f1` present:
//! - `echo f1(.e:"ln -s ../tgt q1":)` (q1), `f1(Ne:…)` (q2),
//!   `f1(oe:"ln …; REPLY=x":)` (q3); `cd f1(.e:…)` (c1), `cd f1(Ne:…)` (c2),
//!   `cd *(-e:…)` (c3), `cd f1(^e:"ln …; false":)` (c4): a qualifier list
//!   whose `e` is not its FIRST letter runs the code.
//! - `X="f1(e:ln -s ../tgt q5:)"; echo ${~X}` (q5) and `echo $~X` (q5b);
//!   `"${~X}"` (double-quoted) did not glob.
//! - `echo "${(j.).e):-${:-\$}(ln -s ../tgt q6)}"` (q6): a `)` inside a
//!   flag delimiter ends the scanner's flag text before the `e`.
//! - `setopt extendedglob; echo f1(#qe:"ln -s ../tgt q11":)` (q11).
//! - `$'a\'b'` before a qualifier (q9) and a `# don't` comment line before
//!   one (q10) ran it too (blastguard: Ask at the fix, see controls).
//!
//! Observed NOT to execute (zsh 5.9 -f): `f1( e:…:)` ("unknown file
//! attribute"), `f1(E:…:)` (no match: `E` is a mode test), `(#qe…)` without
//! `extendedglob`, `"${(P)X}"` / `${(P)X}` with `X` naming a variable
//! holding `a[$(ln …)]`, `$=X` / `${=X}`; an UNQUOTED here-document body
//! `fn f1(e:"ln -s ../tgt h1":) {}` / `let v=(1,2);` (no h1; the body was
//! written verbatim); `arr=(ln -s ../tgt h2)` (no h2, an array assignment);
//! `[[ x =~ ^(err|warn) ]]`.
//!
//! Measured 2026-11-28, base cf33a5b2c vs fix 0cda717c2 (same fixture,
//! base binary built from a detached worktree of cf33a5b2c):
//! - `FP_*` (RED, ignored): Allow at base, Ask at the fix, reason "the line
//!   holds a zsh construct that runs code while a word is expanded …". The
//!   scanner reads the text of UNQUOTED here-document bodies (and of quoted
//!   ones on lines where the body is not removed, e.g. a later `cp`) and
//!   bash/zsh array assignments `x=(…)` and `=~ ^(…)` regexes as code.
//! - `FAILOPEN_CD_COMPOUND_QUALIFIER` (RED, ignored): Ask at base, Allow at
//!   the fix — made LOOSER by this branch (`line_is_relink_data` accepts a
//!   `cd`; `zsh_runs_code` only looks at the letter right after `(`).
//! - glob-subst `${~X}`, `(#qe` under extendedglob, and `(.e` in a `for`
//!   list / input-redirect word (RED, ignored): Allow at base AND fix —
//!   pre-existing, not made looser by this branch; `zsh_runs_code` does not
//!   see them.
//!
//! DOES NOT PROVE: subagent-mode verdicts; reason texts; that the shapes
//! below are the only ones; any zsh option set other than `-f` defaults
//! (plus `setopt extendedglob` where written).

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
    let base = neutral_base::neutral_base().join(format!("verifier-3de4729f-r3-{tag}-{n}"));
    let scratch_root = fixed_tmp::canonical_fixed_temp_root().join(format!(
        "bg-verifier-3de4729f-r3-{tag}-{}-{n}",
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
        "session_id": "verifier-3de4729f-r3-20261128",
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

// ---------------------------------------------------------------- A. FP --

/// Here-documents writing Rust / Markdown / TOML / Python whose body holds
/// `x(e` or `=(`. Run in the worktree (cwd), relative targets. zsh does not
/// glob a here-document body (observed: no h1). Allow at base cf33a5b2c,
/// Ask at 0cda717c2 (measured 2026-11-28).
const FP_HEREDOC_CODE_BODY: &[&str] = &[
    "cat > f.rs <<EOF\nfn f(e: E) {}\nEOF",
    "cat > f.rs <<EOF && cargo test\nfn f(e: E) {}\nEOF",
    "cat >> f.rs <<EOF\nmatch x { Err(e) => 1, _ => foo(e) }\nEOF",
    "cat >> f.rs <<EOF\nmatch x { Err(e) => 1 }\nEOF\ncargo test 2>&1 | tee log.txt",
    "cat > f.rs <<EOF\nlet v=(1,2);\nEOF",
    "cat > n.md <<EOF\nSee docs(e.g. x)\nEOF\ngit add n.md",
    "cat > c.toml <<EOF\n# f(e)\nk = 1\nEOF",
    "cat > c.toml <<EOF\na=(1 2)\nEOF",
    "cat > t.py <<EOF\ndef g(e): return f(e)\nEOF",
    "cat > f.rs <<EOF\nfn f(e: E) {}\nEOF\ncp f.rs g.rs",
    "cat > f.rs <<EOF\nfn f(e: E) {}\nEOF\necho done > log.txt",
    "cat > f.rs <<EOF\nfn f(e: E) {}\nEOF\ncargo fmt && cargo test 2>&1 | tail -5 > out.txt",
    // QUOTED body, kept in the text because of the later `cp`.
    "cat > f.rs <<'EOF'\nfn f(e: E) {}\nEOF\ncp f.rs g.rs",
    "git commit -F - <<EOF\nfix(e2e): x\nEOF\ngit log -1 > out.txt",
];

#[test]
fn fp_heredoc_code_body_with_paren_e_is_allow() {
    let fx = fixture("fp-hd");
    expect(
        &fx,
        &owned(FP_HEREDOC_CODE_BODY),
        &["allow"],
        "here-document body is not globbed",
    );
}

/// Array assignment `x=(…)` and a `=~ ^(…)` regex: no zsh `=( )` process
/// substitution, no glob qualifier (observed: no h2). Allow at base, Ask at
/// 0cda717c2 (measured 2026-11-28).
const FP_ARRAY_AND_REGEX: &[&str] = &[
    "arr=(a b) ; echo ${arr[@]} > out.txt",
    "arr=(a b); cp f.rs g.rs",
    "[[ x =~ ^(err|warn) ]] && echo y > out.txt",
];

#[test]
fn fp_array_assignment_and_regex_are_allow() {
    let fx = fixture("fp-arr");
    expect(
        &fx,
        &owned(FP_ARRAY_AND_REGEX),
        &["allow"],
        "array assignment / regex runs no code",
    );
}

/// Controls: the same kind of text in shapes that are Allow at base AND at
/// 0cda717c2 (measured 2026-11-28) — quoted here-documents whose body is
/// removed, quoted argv.
const CONTROL_ALLOW: &[&str] = &[
    "cat > f.rs <<'EOF'\nfn f(e: E) {}\nEOF",
    "cat > f.rs <<'EOF'\nfn f(e: E) {}\nEOF\ncargo test",
    "cat > f.rs <<'EOF' && cargo test\nfn f(e: E) {}\nEOF",
    "cat >> f.rs <<'EOF'\nmatch x { Err(e) => 1, _ => foo(e) }\nEOF",
    "cat >> f.rs <<'EOF'\nmatch x { Err(e) => 1 }\nEOF\ncargo test 2>&1 | tee log.txt",
    "cat > f.rs <<'EOF'\nlet v=(1,2);\nEOF\ncargo test",
    "cat > n.md <<'EOF'\nSee docs(e.g. x) and x+(y) and g(+1)\nEOF",
    "cat > n.md <<EOF\nSee docs(e.g. x) and x+(y) and g(+1)\nEOF",
    "cat > c.toml <<'EOF'\n# f(e)\nk = 1\nEOF",
    "cat > t.py <<'EOF'\ndef g(e): return f(e)\nEOF",
    "cat > f.rs <<'EOF'\nfn f(e: E) {}\nEOF\necho done > log.txt",
    "git commit -m \"fix(e2e): x\" > out.txt",
    "git commit -F - <<'EOF'\nfix(e2e): x\nEOF",
    "git commit -F - <<EOF\nfix(e2e): x\nEOF",
    "echo \"see (e.g.)\" > n.txt",
    "printf '%s' \"a=(b)\" > n.txt",
    "jq '.a|(e)' in.json > out.json",
    "python3 -c \"print(f(e))\" > out.txt",
    "echo hi # it's fine\necho x > out.txt",
    "cargo test 2>&1 | tee log.txt",
];

#[test]
fn control_paren_e_text_in_data_stays_allow() {
    let fx = fixture("ctl-allow");
    expect(
        &fx,
        &owned(CONTROL_ALLOW),
        &["allow"],
        "Allow at base and fix",
    );
}

// --------------------------------------------------------- B. fail-open --

/// `cd` operand with a qualifier list whose `e` is not the first letter
/// (observed executing: c1-c4). Absolute paths, so only the relink layer
/// can see the `ln`. FAIL-OPEN made looser by this branch: Ask at base
/// cf33a5b2c ("… may create or rename an entry on this path and is not
/// modelled"), Allow at 0cda717c2 (measured 2026-11-28 for `(.e`).
fn failopen_cd_compound_qualifier(s: &str) -> Vec<String> {
    vec![
        format!("cd *(.e:' ln -s /etc {s}/d ':); echo x > {s}/d/hosts"),
        format!("cd *(Ne:' ln -s /etc {s}/d ':); echo x > {s}/d/hosts"),
        format!("cd *(-e:' ln -s /etc {s}/d ':); echo x > {s}/d/hosts"),
        format!("cd *(^e:' ln -s /etc {s}/d; false ':); echo x > {s}/d/hosts"),
        format!("cd *(oe:' ln -s /etc {s}/d; REPLY=x ':); echo x > {s}/d/hosts"),
        format!("cd *(.e:' busybox ln -s /etc {s}/d ':); echo x > {s}/d/hosts"),
        format!("cd {s}/*(.e:' ln -s /etc {s}/d ':); echo x > {s}/d/hosts"),
        format!("cd *(.e:' ln -s ~/.claude {s}/d ':); echo x > {s}/d/settings.json"),
    ]
}

#[test]
fn failopen_cd_compound_qualifier_stays_not_allow() {
    let fx = fixture("fo-cd");
    expect(
        &fx,
        &failopen_cd_compound_qualifier(&fx.s()),
        &["ask", "deny"],
        "cd compound glob qualifier runs ln",
    );
}

/// zsh `${~X}` / `$~X` (GLOB_SUBST) on a single-quoted qualifier (observed:
/// q5, q5b). PRE-EXISTING: Allow at base AND 0cda717c2 (measured 2026-11-28).
fn failopen_glob_subst(s: &str) -> Vec<String> {
    vec![
        format!("X='*(e:ln -s /etc {s}/d:)'; echo ${{~X}}; echo x > {s}/d/hosts"),
        format!("X='*(e:ln -s /etc {s}/d:)'; echo $~X; echo x > {s}/d/hosts"),
        format!("X='*(e:ln -s /etc {s}/d:)'; cd ${{~X}}; echo x > {s}/d/hosts"),
    ]
}

#[test]
fn failopen_glob_subst_stays_not_allow() {
    let fx = fixture("fo-subst");
    expect(
        &fx,
        &failopen_glob_subst(&fx.s()),
        &["ask", "deny"],
        "${~X} runs ln",
    );
}

/// `(#qe:…:)` with `setopt extendedglob` on the line (observed: q11).
/// PRE-EXISTING: Allow at base AND 0cda717c2 (measured 2026-11-28).
fn failopen_hash_q(s: &str) -> Vec<String> {
    vec![
        format!("setopt extendedglob; echo *(#qe:\"ln -s /etc {s}/d\":); echo x > {s}/d/hosts"),
        format!("setopt extendedglob; cd *(#qe:\"ln -s /etc {s}/d\":); echo x > {s}/d/hosts"),
    ]
}

#[test]
fn failopen_hash_q_stays_not_allow() {
    let fx = fixture("fo-hashq");
    expect(
        &fx,
        &failopen_hash_q(&fx.s()),
        &["ask", "deny"],
        "(#qe) runs ln",
    );
}

/// Controls: executing shapes that are NOT Allow at 0cda717c2 (measured
/// 2026-11-28): `echo` with compound qualifiers, the `(j.).e)` flag text,
/// and quote / comment confusions before a qualifier.
fn control_not_allow(s: &str) -> Vec<String> {
    vec![
        format!("echo *(.e:' ln -s /etc {s}/d ':); echo x > {s}/d/hosts"),
        format!("echo *(Ne:' ln -s /etc {s}/d ':); echo x > {s}/d/hosts"),
        format!("echo *(oe:' ln -s /etc {s}/d; REPLY=x ':); echo x > {s}/d/hosts"),
        format!("echo *(.e:' busybox ln -s /etc {s}/d ':); echo x > {s}/d/hosts"),
        format!("echo \"${{(j.).e):-${{:-\\$}}(ln -s /etc {s}/d)}}\"; echo x > {s}/d/hosts"),
        format!("cd \"${{(j.).e):-${{:-\\$}}(ln -s /etc {s}/d)}}\"; echo x > {s}/d/hosts"),
        format!("echo $'a\\'b' *(e:\"ln -s /etc {s}/d\":); echo x > {s}/d/hosts"),
        format!("echo $'a\\'b'; cd *(e:\"ln -s /etc {s}/d\":); echo x > {s}/d/hosts"),
        format!("echo hi # don't\necho *(e:\"ln -s /etc {s}/d\":); echo x > {s}/d/hosts"),
        format!("echo hi # don't\ncd *(e:\"ln -s /etc {s}/d\":); echo x > {s}/d/hosts"),
        format!("echo *(e:\"ln -s /etc {s}/d\":); echo x > {s}/d/hosts"),
        format!("cd *(e:' ln -s /etc {s}/d ':); echo x > {s}/d/hosts"),
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

/// System target through the compound qualifier: Deny.
#[test]
fn control_cd_compound_qualifier_system_target_stays_deny() {
    let fx = fixture("ctl-sys");
    let cmds = vec!["cd *(.e:' ln -s /etc d ':); echo x > /etc/hosts".to_string()];
    expect(&fx, &cmds, &["deny"], "system harm");
}

/// The compound qualifier `(.e` in a `for` word list and an input-redirect
/// word (observed executing: r1, r2): `zsh_runs_code` misses it, so the
/// branch's new guard does not reach these. PRE-EXISTING: Allow at base AND
/// 0cda717c2 (measured 2026-11-28).
fn failopen_for_redirect_compound(s: &str) -> Vec<String> {
    vec![
        format!("for i in *(.e:' ln -s /etc {s}/d ':); do echo; done; echo x > {s}/d/hosts"),
        format!("cat < *(.e:' ln -s /etc {s}/d ':); echo x > {s}/d/hosts"),
    ]
}

#[test]
fn failopen_for_redirect_compound_qualifier_stays_not_allow() {
    let fx = fixture("fo-forredir");
    expect(
        &fx,
        &failopen_for_redirect_compound(&fx.s()),
        &["ask", "deny"],
        "(.e) in for list / redirect word runs ln",
    );
}

/// A relinked `cp` operand after the compound qualifier: Ask at base and
/// 0cda717c2 (measured 2026-11-28).
#[test]
fn control_cd_compound_qualifier_then_cp_stays_not_allow() {
    let fx = fixture("ctl-cp");
    let s = fx.s();
    let cmds = vec![format!(
        "cd *(.e:' ln -s /etc {s}/d ':); cp {s}/in.json {s}/d/hosts"
    )];
    expect(&fx, &cmds, &["ask", "deny"], "cp operand after relink");
}
