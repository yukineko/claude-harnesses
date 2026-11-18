#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (2026-11-18): bucket A of the 2026-11-18 block-log FP replay — every
//! value is statically determined by the command text, yet a legacy reading
//! still refuses it.
//!
//! Written by an independent test author (not the implementer; CLAUDE.md
//! §2(a)). The expectation comes from the user ruling "blastguard must
//! resolve statically" (2026-11-14): a variable literally assigned in an
//! earlier, unconditionally executed statement of the same command is
//! resolved and judged on the resolved value; only values that exist only at
//! run time (`$(…)`, unassigned, conditional / subshell assignment) stay
//! undetermined -> restrictive. SUBSET.md Table A.3 (`$x` named: "literal iff
//! bound to a literal on the line") and A.2 (`ForClause` literal list
//! unrolled) say the same.
//!
//! Clusters covered (replay at main 04d9b8336):
//!   * `'> X' destroys contents and X contains a shell expansion`
//!     (`reversible.rs:286`, built at `detect.rs:1041`), 12 rows: a redirect
//!     target spelled through a literal-bound variable / literal for-list.
//!   * `cannot determine whether this git add/commit is whole-tree staging`
//!     (`detect/git_stage.rs:83`), 7 rows: `T=<path>; git add $T`,
//!     backslash-newline continuations, bounded globs.
//!
//! Method (as `assignment_after_closed_and_or_list_20261113.rs`): each FP
//! case first MEASURES the literal spelling of the same command in main and
//! subagent mode and pins it to the expected decision, then requires the
//! variable spelling to answer exactly the same in each mode (parity). The
//! shapes are sanitized from the real rows: every target is a non-existent
//! sentinel under `/tmp/bg-sr-sentinel-*` (Scratch) or inside the fixture's
//! own linked worktree, or an `/etc/bg-sentinel-*` sentinel for the System
//! control. The hook only JUDGES the text; nothing is executed.
//!
//! Controls (must stay NOT Allow): a value from `$(…)`, a value reassigned
//! inside an `if` body, `S=/etc; … > $S/bg-sentinel-hosts` (System -> Deny
//! in both modes, parity with the literal), and `git add $T` whose value is
//! whole-tree (`.`, `-A`, `*`), from `$(…)`, or conditionally reassigned.
//!
//! Harness: `ast_effect_rules_20261117.rs` (hermetic `env_clear`, fixture
//! `HOME`, `CLAUDE_PROJECT_DIR` = payload cwd, `BLASTGUARD_APPROVALS_DIR`,
//! no `TMPDIR`), run in main mode and in subagent mode (`agent_id`).
//!
//! Replay finding (2026-11-18, the real rows judged by the binary at
//! origin/main 04d9b8336, nothing executed): of the 12 redirect rows, most
//! bind the target variable INSIDE an and-or list (`cd X && D=<p>; …`,
//! `cd X && . env && S=<p>; …`: L2, L10, L52, L59, L66, L69, L306, L322) —
//! the variable is unset when `cd` fails, so that is a conditional binding,
//! pinned here as control `rc5`, not an FP case. L7 binds `S` before a
//! `.` (control `rc6`, SUBSET.md A.5). The FP shapes that DID reproduce are
//! `r21` (L35: for-list variable after `.`) and `r24` (L284). The
//! `;`-separated sanitized shapes (`r01`–`r20`, `r22`, `r23`, `r25`, `r26`)
//! answer as their literal spelling today and stay as GREEN parity guards.
//!
//! DOES NOT PROVE: which rule produced a decision (only the decision is
//! asserted, not the reason); assignments inside an and-or list
//! (`cd X && S=/p; … > $S/x` — the real rows L10/L35 have that shape, but
//! the existing test file pins it as a must-stay-refused control, so it is
//! left to a ruling); `. "$HOME/.cargo/env"` prefixes present in many real
//! rows (refused by rulings 13/16 on its own); unquoted values that contain
//! IFS whitespace or glob characters; values with `..`; behaviour with
//! `TMPDIR` set; the `redir-relative-after-cd` / `sed-i-unresolved` /
//! `safe-root-confined-ask` / `norule-protected` bucket-A clusters. Controls
//! pin "not Allow" (main) / Deny (subagent), not the exact main Ask/Deny split.

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/neutral_base.rs"]
mod neutral_base;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Fx {
    base: PathBuf,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

impl Fx {
    fn home(&self) -> PathBuf {
        self.base.join("home")
    }
    /// The plain project dir (no git): the default payload cwd.
    fn proj(&self) -> PathBuf {
        self.base.join("src").join("proj")
    }
    /// A linked-worktree-shaped checkout under the hermetic
    /// `$HOME/.condukt/worktrees` storage root (a `.git` FILE starting
    /// `gitdir:`, what `git worktree add` writes).
    fn own_worktree(&self) -> PathBuf {
        let wt = self
            .home()
            .join(".condukt")
            .join("worktrees")
            .join("bg-sr-wt");
        std::fs::create_dir_all(wt.join("sub")).unwrap();
        std::fs::write(
            wt.join(".git"),
            "gitdir: /nonexistent/bg-sr/.git/worktrees/bg-sr-wt\n",
        )
        .unwrap();
        wt
    }
}

fn fixture(tag: &str) -> Fx {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let base = neutral_base::neutral_base().join(format!(
        "static-resolve-{tag}-{}",
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("home").join(".cache")).unwrap();
    std::fs::create_dir_all(base.join("src").join("proj")).unwrap();
    std::fs::create_dir_all(base.join("store")).unwrap();
    Fx { base }
}

/// Run the hook once; `(decision, reason)`.
///
/// Order is load-bearing: (1) a non-zero / signalled exit panics; (2) only
/// then is an EMPTY stdout read as Allow; (3) non-JSON stdout panics.
fn run_as(fx: &Fx, cwd: &Path, command: &str, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "static-resolve-fp-20261118",
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
        .env("HOME", fx.home())
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env("CLAUDE_PROJECT_DIR", cwd)
        .env("BLASTGUARD_APPROVALS_DIR", fx.base.join("store"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
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
    let d = h["permissionDecision"].as_str().unwrap_or("?").to_string();
    let r = h["permissionDecisionReason"]
        .as_str()
        .unwrap_or("")
        .to_string();
    println!("[sub={subagent}] {command:?} => {d} ({r})");
    (d, r)
}

/// Pin the literal spelling to `expected` in both modes, then require the
/// variable spelling to answer exactly as the literal did in each mode.
#[track_caller]
fn parity_at(fx: &Fx, cwd: &Path, literal: &str, expected: &str, variable: &str) {
    for subagent in [false, true] {
        let (lit, lr) = run_as(fx, cwd, literal, subagent);
        assert_eq!(
            lit, expected,
            "subagent={subagent}: control (literal spelling) {literal:?} must be {expected}; got {lit} ({lr})"
        );
        let (var, vr) = run_as(fx, cwd, variable, subagent);
        assert_eq!(
            var, lit,
            "subagent={subagent}: {variable:?} must answer as the literal {literal:?} ({lit}); got {var} ({vr})"
        );
    }
}

/// Main: Ask or stricter; subagent: Deny.
#[track_caller]
fn restrict_at(fx: &Fx, cwd: &Path, command: &str) {
    let (dm, rm) = run_as(fx, cwd, command, false);
    assert!(
        dm == "ask" || dm == "deny",
        "main: {command:?} expected Ask or stricter, got {dm} ({rm})"
    );
    let (ds, rs) = run_as(fx, cwd, command, true);
    assert_eq!(
        ds, "deny",
        "subagent: {command:?} expected Deny, got {ds} ({rs})"
    );
}

#[track_caller]
fn allow_at(fx: &Fx, cwd: &Path, command: &str) {
    for subagent in [false, true] {
        let (d, r) = run_as(fx, cwd, command, subagent);
        assert_eq!(
            d, "allow",
            "subagent={subagent}: {command:?} expected Allow, got {d} ({r})"
        );
    }
}

fn scratch(tag: &str) -> String {
    format!("/tmp/bg-sr-sentinel-{tag}")
}

// ---------------------------------------------------------------------------
// R — `'> X' … contains a shell expansion` (reversible.rs:286) — parity
// ---------------------------------------------------------------------------

/// Rows L7/L52/L284 shape: `S=<scratch>; cmd > $S/x`.
#[test]
fn r01_redirect_through_literal_bound_var() {
    let fx = fixture("r01");
    let s = scratch("r01");
    parity_at(
        &fx,
        &fx.proj(),
        &format!("echo hi > {s}/out.log"),
        "allow",
        &format!("S={s}; echo hi > $S/out.log"),
    );
}

/// Braced spelling `${S}/x`.
#[test]
fn r02_redirect_through_braced_var() {
    let fx = fixture("r02");
    let s = scratch("r02");
    parity_at(
        &fx,
        &fx.proj(),
        &format!("echo hi > {s}/out.log"),
        "allow",
        &format!("S={s}; echo hi > ${{S}}/out.log"),
    );
}

/// Double-quoted spelling `"$S/x"` (no field splitting at all).
#[test]
fn r03_redirect_through_double_quoted_var() {
    let fx = fixture("r03");
    let s = scratch("r03");
    parity_at(
        &fx,
        &fx.proj(),
        &format!("echo hi > \"{s}/out.log\""),
        "allow",
        &format!("S={s}; echo hi > \"$S/out.log\""),
    );
}

/// Rows L66/L306 shape: `O=<scratch>; cargo … > $O/x 2>&1` (here `echo`).
#[test]
fn r04_redirect_with_stderr_dup_through_var() {
    let fx = fixture("r04");
    let s = scratch("r04");
    parity_at(
        &fx,
        &fx.proj(),
        &format!("echo hi > {s}/test.log 2>&1"),
        "allow",
        &format!("O={s}; echo hi > $O/test.log 2>&1"),
    );
}

/// Row L284 shape: `mkdir -p $S/ws && … > $S/…`.
#[test]
fn r05_mkdir_then_redirect_through_var() {
    let fx = fixture("r05");
    let s = scratch("r05");
    parity_at(
        &fx,
        &fx.proj(),
        &format!("mkdir -p {s}/ws && echo hi > {s}/ws/out.log"),
        "allow",
        &format!("S={s}; mkdir -p $S/ws && echo hi > $S/ws/out.log"),
    );
}

/// Row L35 shape: a literal for-list; the literal spelling is the unrolled
/// loop (SUBSET.md A.2: literal list unrolled).
#[test]
fn r06_redirect_in_literal_for_list() {
    let fx = fixture("r06");
    let s = scratch("r06");
    parity_at(
        &fx,
        &fx.proj(),
        &format!("echo hi > {s}/a.log; echo hi > {s}/b.log"),
        "allow",
        &format!("S={s}; for f in a b; do echo hi > $S/$f.log; done"),
    );
}

/// Rows L69/L112 shape: here-document to `cat > $D/file`.
#[test]
fn r07_heredoc_cat_through_var() {
    let fx = fixture("r07");
    let s = scratch("r07");
    parity_at(
        &fx,
        &fx.proj(),
        &format!("cat > {s}/probe.rs <<'EOF'\nfn main() {{}}\nEOF"),
        "allow",
        &format!("D={s}; cat > $D/probe.rs <<'EOF'\nfn main() {{}}\nEOF"),
    );
}

/// Rows L2/L59/L322 shape, with the `cd` CLOSED by `;`: a literal
/// `cd <own worktree>;` first, then `D=<scratch>; … > $D/x`. The real rows
/// spell it `cd <wt> && D=<scratch>; …`, where `D=` is inside the and-or
/// list (runs only if `cd` succeeds); that spelling is pinned as a
/// must-stay-refused control in `assignment_after_closed_and_or_list_20261113.rs`
/// and again in `rc5` below, so it is NOT an FP case here.
#[test]
fn r08_literal_cd_then_var_redirect() {
    let fx = fixture("r08");
    let wt = fx.own_worktree();
    let w = wt.display().to_string();
    let s = scratch("r08");
    parity_at(
        &fx,
        &wt,
        &format!("cd {w}; echo hi > {s}/run.json"),
        "allow",
        &format!("cd {w}; D={s}; echo hi > $D/run.json"),
    );
}

/// Two literal-bound variables concatenated in one target.
#[test]
fn r09_two_vars_concatenated() {
    let fx = fixture("r09");
    let s = scratch("r09");
    parity_at(
        &fx,
        &fx.proj(),
        &format!("echo hi > {s}/out.log"),
        "allow",
        &format!("R={s}; N=out; echo hi > $R/$N.log"),
    );
}

/// Own-worktree target (Table C OwnWorktree x Truncate) through a variable.
#[test]
fn r10_own_worktree_target_through_var() {
    let fx = fixture("r10");
    let wt = fx.own_worktree();
    let t = wt.join("sub").display().to_string();
    parity_at(
        &fx,
        &wt,
        &format!("echo hi > {t}/out.log"),
        "allow",
        &format!("W={t}; echo hi > $W/out.log"),
    );
}

/// Row L7 shape without the `.` prefix: `S=<scratch>; cd <wt> && cmd >
/// $S/x 2>&1` (the assignment precedes the literal `cd`).
#[test]
fn r11_var_then_literal_cd_and_redirect() {
    let fx = fixture("r11");
    let wt = fx.own_worktree();
    let w = wt.display().to_string();
    let s = scratch("r11");
    parity_at(
        &fx,
        &wt,
        &format!("cd {w} && cargo test -p x > {s}/test.log 2>&1"),
        "allow",
        &format!("S={s}; cd {w} && cargo test -p x > $S/test.log 2>&1"),
    );
}

/// Newline-separated statements (the real rows are multi-line).
#[test]
fn r12_newline_separated_assignment() {
    let fx = fixture("r12");
    let s = scratch("r12");
    parity_at(
        &fx,
        &fx.proj(),
        &format!("mkdir -p {s}\necho hi > {s}/out.log"),
        "allow",
        &format!("S={s}\nmkdir -p $S\necho hi > $S/out.log"),
    );
}

/// Rows L2/L59 shape: the redirect sits inside an `if` body / `else` body
/// whose condition is run time, but `D` itself was bound unconditionally
/// before the `if`, so the target is the same on every path.
#[test]
fn r13_redirect_inside_if_body_var_bound_before() {
    let fx = fixture("r13");
    let s = scratch("r13");
    parity_at(
        &fx,
        &fx.proj(),
        &format!(
            "if command -v jq >/dev/null; then jq . {s}/in.json > {s}/out.json; else cp {s}/in.json {s}/out.json; fi"
        ),
        "allow",
        &format!(
            "D={s}; if command -v jq >/dev/null; then jq . $D/in.json > $D/out.json; else cp $D/in.json $D/out.json; fi"
        ),
    );
}

/// Row L7 shape (without its `. "$HOME/.cargo/env"`): the redirect is on the
/// last element of a pipeline.
#[test]
fn r14_redirect_at_pipeline_end_through_var() {
    let fx = fixture("r14");
    let wt = fx.own_worktree();
    let w = wt.display().to_string();
    let s = scratch("r14");
    parity_at(
        &fx,
        &wt,
        &format!("cd {w} && cargo test -p x 2>&1 | grep -E 'main=' | sort > {s}/base.txt"),
        "allow",
        &format!("S={s}; cd {w} && cargo test -p x 2>&1 | grep -E 'main=' | sort > $S/base.txt"),
    );
}

/// Row L35 shape: a literal for-list whose variable is part of the target
/// file name, plus a second literal-bound variable in an option value.
#[test]
fn r15_for_list_var_in_target_name() {
    let fx = fixture("r15");
    let wt = fx.own_worktree();
    let m = wt.join("Cargo.toml").display().to_string();
    let s = scratch("r15");
    parity_at(
        &fx,
        &wt,
        &format!(
            "cargo test --manifest-path {m} --test ta > {s}/ta.txt 2>&1; cargo test --manifest-path {m} --test tb > {s}/tb.txt 2>&1"
        ),
        "allow",
        &format!(
            "M={m}; S={s}; for t in ta tb; do cargo test --manifest-path $M --test $t > $S/$t.txt 2>&1; done"
        ),
    );
}

/// Row L69 shape (assignment separated by `;` instead of `&&`): a relative
/// own-worktree target through a variable, `>` then `cat >> $f <<'EOF'`.
#[test]
fn r16_relative_own_worktree_target_through_var() {
    let fx = fixture("r16");
    let wt = fx.own_worktree();
    parity_at(
        &fx,
        &wt,
        "printf '%s\\n' '//! x' > sub/probe_fp.rs && cat >> sub/probe_fp.rs <<'EOF'\nconst X: u8 = 1;\nEOF",
        "allow",
        "f=sub/probe_fp.rs; printf '%s\\n' '//! x' > $f && cat >> $f <<'EOF'\nconst X: u8 = 1;\nEOF",
    );
}

/// Row L322 shape: the target variable is literal; ANOTHER variable on the
/// line (`RID`) is run time, which must not taint the target.
#[test]
fn r17_target_var_literal_other_var_runtime() {
    let fx = fixture("r17");
    let s = scratch("r17");
    parity_at(
        &fx,
        &fx.proj(),
        &format!("RID=$(date +%s) && echo $RID; echo \"$RID\" > {s}/rid"),
        "allow",
        &format!("D={s}; RID=$(date +%s) && echo $RID; echo \"$RID\" > $D/rid"),
    );
}

/// Row L52 shape: `cmd > $S/x 2>/dev/null || cmd > $S/x` (S bound first).
#[test]
fn r18_or_list_redirects_through_var() {
    let fx = fixture("r18");
    let s = scratch("r18");
    parity_at(
        &fx,
        &fx.proj(),
        &format!("echo a > {s}/bl.json 2>/dev/null || echo b > {s}/bl.json"),
        "allow",
        &format!("S={s}; echo a > $S/bl.json 2>/dev/null || echo b > $S/bl.json"),
    );
}

/// Row L284 shape: `cd <proj>; S=<scratch>; mkdir -p $S/ws && … | tar -x -C
/// $S/ws && ls $S/ws; cat > $S/ws/t.rs <<'EOF' … EOF`.
#[test]
fn r19_cd_then_var_mkdir_tar_heredoc_cat() {
    let fx = fixture("r19");
    let p = fx.proj().display().to_string();
    let s = scratch("r19");
    parity_at(
        &fx,
        &fx.proj(),
        &format!(
            "cd {p}; mkdir -p {s}/ws && git archive HEAD Cargo.toml | tar -x -C {s}/ws && ls {s}/ws; cat > {s}/ws/t.rs <<'EOF'\n//! RED probe\nEOF"
        ),
        "allow",
        &format!(
            "cd {p}; S={s}; mkdir -p $S/ws && git archive HEAD Cargo.toml | tar -x -C $S/ws && ls $S/ws; cat > $S/ws/t.rs <<'EOF'\n//! RED probe\nEOF"
        ),
    );
}

/// Row L35 shape (its `. "$HOME/.cargo/env"` replaced by `true`): an
/// and-or list binds `M` conditionally, but the `;` closes it and `S` — the
/// only variable in the redirect target besides the loop variable — is
/// bound unconditionally afterwards.
#[test]
fn r20_conditional_other_var_closed_then_target_var() {
    let fx = fixture("r20");
    let wt = fx.own_worktree();
    let w = wt.display().to_string();
    let m = wt.join("Cargo.toml").display().to_string();
    let s = scratch("r20");
    parity_at(
        &fx,
        &wt,
        &format!(
            "cd {w} && true && M={m}; cargo test --manifest-path $M --test ta > {s}/ta.txt 2>&1; cargo test --manifest-path $M --test tb > {s}/tb.txt 2>&1"
        ),
        "allow",
        &format!(
            "cd {w} && true && M={m}; S={s}; for t in ta tb; do cargo test --manifest-path $M --test $t > $S/$t.txt 2>&1; done"
        ),
    );
}

/// Row L35, bisected (2026-11-18 replay of the real row through the binary
/// at origin/main): removing `. "$HOME/.cargo/env" &&` turns Deny into
/// Allow, and the refusal names `'> <scratch>/$t.txt'` — `S` IS resolved,
/// only the literal for-list variable `t` is not. A `for t in <words>`
/// binds `t` to literal words whatever the sourced file did, so the parity
/// spelling is the unrolled loop after the same `.`.
// test-weakening-justified: new RED for an unfixed FP, observed failing at origin/main 04d9b8336 (2026-11-18)
#[test]
fn r21_for_list_var_after_dot_source() {
    let fx = fixture("r21");
    let s = scratch("r21");
    parity_at(
        &fx,
        &fx.proj(),
        &format!(". \"$HOME/.cargo/env\" && true; S={s}; echo x > $S/ta.txt; echo x > $S/tb.txt"),
        "allow",
        &format!(
            ". \"$HOME/.cargo/env\" && true; S={s}; for t in ta tb; do echo x > $S/$t.txt; done"
        ),
    );
}

/// Control for `r21` (user ruling 2026-11-18): only the literal
/// `. "$HOME/.cargo/env"` leaves the AST variable state determined. Any
/// other sourced file (`. ./env.sh`) may redefine `S` / `t` (or `echo`), so
/// the same for-loop shape after it stays not-Allow (main) / Deny (subagent).
#[test]
fn r21_control_for_list_var_after_other_source_stays_restricted() {
    let fx = fixture("r21c");
    let s = scratch("r21c");
    restrict_at(
        &fx,
        &fx.proj(),
        &format!(". ./env.sh && true; S={s}; for t in ta tb; do echo x > $S/$t.txt; done"),
    );
}

/// Row L284, bisected: the refusal appears once the quoted here-document
/// body contains a line with an apostrophe (`//!   => … line 2117's text`);
/// with the first 5 body lines it is Allow. The body of a QUOTED
/// here-document is data, and `S` is bound before it, so the target is
/// determined.
#[test]
fn r22_heredoc_cat_through_var_body_with_apostrophe() {
    let fx = fixture("r22");
    let s = scratch("r22");
    parity_at(
        &fx,
        &fx.proj(),
        &format!(
            "mkdir -p {s}/ws; cat > {s}/ws/t.rs <<'EOF'\n//! note\n//!   => the hook decided on line 7's text, not on line 9's.\nfn f() {{}}\nEOF"
        ),
        "allow",
        &format!(
            "S={s}; mkdir -p $S/ws; cat > $S/ws/t.rs <<'EOF'\n//! note\n//!   => the hook decided on line 7's text, not on line 9's.\nfn f() {{}}\nEOF"
        ),
    );
}

/// Row L284 second half: a `. "$HOME/.cargo/env"` AFTER the redirect (it
/// cannot change a value that was already used). No here-document.
#[test]
fn r23_dot_source_after_redirect_through_var() {
    let fx = fixture("r23");
    let s = scratch("r23");
    parity_at(
        &fx,
        &fx.proj(),
        &format!("echo x > {s}/t.rs; cd {s} && . \"$HOME/.cargo/env\" && true"),
        "allow",
        &format!("S={s}; echo x > $S/t.rs; cd $S && . \"$HOME/.cargo/env\" && true"),
    );
}

/// Row L284, minimised by replay (2026-11-18, binary at origin/main): the
/// refusal needs ALL of (a) a quoted here-document body holding a
/// `#![allow(…)]` line and apostrophes (`hook's`, `line 7's … line 9's`),
/// (b) a later `. "$HOME/.cargo/env"`, and (c) a later
/// `CARGO_TARGET_DIR=$S/target cargo test …`; dropping any one of them is
/// Allow. The body is data (quoted delimiter) and `S` is bound before it.
// test-weakening-justified: new RED for an unfixed FP, observed failing at origin/main 04d9b8336 (2026-11-18)
#[test]
fn r24_heredoc_apostrophes_then_dot_source_and_env_prefix_after() {
    let fx = fixture("r24");
    let s = scratch("r24");
    let body = "#![allow(clippy::panic)]\n//! RED: the hook's Stop path\n//!   => it decided on line 7's text, not on line 9's.";
    parity_at(
        &fx,
        &fx.proj(),
        &format!(
            "mkdir -p {s}/ws; cat > {s}/ws/t.rs <<'EOF'\n{body}\nEOF\ncd {s}/ws && . \"$HOME/.cargo/env\" && CARGO_TARGET_DIR={s}/target cargo test -p x --test t 2>&1 | tail -30"
        ),
        "allow",
        &format!(
            "S={s}; mkdir -p $S/ws; cat > $S/ws/t.rs <<'EOF'\n{body}\nEOF\ncd $S/ws && . \"$HOME/.cargo/env\" && CARGO_TARGET_DIR=$S/target cargo test -p x --test t 2>&1 | tail -30"
        ),
    );
}

/// Row L112 tail shape: `. "$HOME/.cargo/env"; S=<scratch>;
/// CARGO_TARGET_DIR=$S/tfix cargo test … | grep … > $S/fix.txt` (S bound
/// after the `.`, then used in a prefix assignment and a redirect).
#[test]
fn r25_dot_then_var_env_prefix_and_redirect() {
    let fx = fixture("r25");
    let s = scratch("r25");
    parity_at(
        &fx,
        &fx.proj(),
        &format!(
            ". \"$HOME/.cargo/env\"; CARGO_TARGET_DIR={s}/tfix cargo test -p x -- --nocapture 2>&1 | grep -E \"PROBE|error\" > {s}/fix.txt"
        ),
        "allow",
        &format!(
            ". \"$HOME/.cargo/env\"; S={s}; CARGO_TARGET_DIR=$S/tfix cargo test -p x -- --nocapture 2>&1 | grep -E \"PROBE|error\" > $S/fix.txt"
        ),
    );
}

/// Row L112 whole shape: a quoted here-document written into the own
/// worktree whose body has apostrophes and a non-terminator `EOF{after}`
/// text, then the r25 tail.
#[test]
fn r26_heredoc_body_then_dot_var_env_prefix_and_redirect() {
    let fx = fixture("r26");
    let wt = fx.own_worktree();
    let s = scratch("r26");
    let body = "#![allow(clippy::panic)]\n//! Verifier probes: each probe's command must NOT be allowed.\nfn h(body: &str, after: &str) -> String {\n    format!(\"python3 - <<'EOF'\\n{body}\\nEOF{after}\")\n}";
    parity_at(
        &fx,
        &wt,
        &format!(
            "cat > sub/probes.rs <<'EOF'\n{body}\nEOF\n. \"$HOME/.cargo/env\"; CARGO_TARGET_DIR={s}/tfix cargo test -p x 2>&1 | grep -E \"PROBE\" > {s}/fix.txt; cat {s}/fix.txt"
        ),
        "allow",
        &format!(
            "cat > sub/probes.rs <<'EOF'\n{body}\nEOF\n. \"$HOME/.cargo/env\"; S={s}; CARGO_TARGET_DIR=$S/tfix cargo test -p x 2>&1 | grep -E \"PROBE\" > $S/fix.txt; cat $S/fix.txt"
        ),
    );
}

// ---------------------------------------------------------------------------
// R controls — must stay NOT Allow
// ---------------------------------------------------------------------------

/// The value exists only at run time.
#[test]
fn rc1_value_from_command_substitution_restricts() {
    let fx = fixture("rc1");
    restrict_at(&fx, &fx.proj(), "S=$(mktemp -d); echo hi > $S/out.log");
}

/// The value is reassigned inside an `if` body whose condition is not
/// literal: the target is either the scratch dir or `/etc`.
#[test]
fn rc2_conditionally_reassigned_value_restricts() {
    let fx = fixture("rc2");
    let s = scratch("rc2");
    restrict_at(
        &fx,
        &fx.proj(),
        &format!(
            "S={s}; if test -e /tmp/bg-sr-sentinel-flag; then S=/etc; fi; echo hi > $S/bg-sentinel-hosts"
        ),
    );
}

/// System target through a literal-bound variable: Deny in both modes,
/// exactly as the literal spelling (ruling 3).
#[test]
fn rc3_system_target_through_var_denies() {
    let fx = fixture("rc3");
    parity_at(
        &fx,
        &fx.proj(),
        "echo hi > /etc/bg-sentinel-hosts",
        "deny",
        "S=/etc; echo hi > $S/bg-sentinel-hosts",
    );
}

/// System target reached through a literal for-list.
#[test]
fn rc4_system_target_in_for_list_denies() {
    let fx = fixture("rc4");
    parity_at(
        &fx,
        &fx.proj(),
        "echo hi > /etc/bg-sentinel-a",
        "deny",
        "for d in /etc; do echo hi > $d/bg-sentinel-a; done",
    );
}

/// The real-row spelling `cd <wt> && D=<scratch>; … > $D/x`: `D=` runs only
/// when `cd` succeeds, so `$D` may be empty and the target `/run.json`.
#[test]
fn rc5_assignment_inside_and_or_list_restricts() {
    let fx = fixture("rc5");
    let wt = fx.own_worktree();
    let w = wt.display().to_string();
    let s = scratch("rc5");
    restrict_at(&fx, &wt, &format!("cd {w} && D={s}; echo hi > $D/run.json"));
}

/// Row L7 shape: `S` is bound BEFORE `. "$HOME/.cargo/env"`; the sourced
/// file (not in the command text) may reassign it. SUBSET.md A.5: `.` /
/// `source` make the state opaque (later values Undetermined). So the value
/// at the redirect is NOT text-determined and must stay refused.
#[test]
fn rc6_var_bound_before_dot_source_restricts() {
    let fx = fixture("rc6");
    let s = scratch("rc6");
    restrict_at(
        &fx,
        &fx.proj(),
        &format!("S={s}; . \"$HOME/.cargo/env\"; echo x > $S/base.txt"),
    );
}

// ---------------------------------------------------------------------------
// G — `cannot determine whether this git add/commit is whole-tree staging`
// (detect/git_stage.rs:83) — parity
// ---------------------------------------------------------------------------

/// Row L275 shape: `T=<explicit path>; git add $T`.
#[test]
fn g01_git_add_literal_bound_var() {
    let fx = fixture("g01");
    let wt = fx.own_worktree();
    parity_at(
        &fx,
        &wt,
        "git add crates/x/tests/backlog_x.rs",
        "allow",
        "T=crates/x/tests/backlog_x.rs; git add $T",
    );
}

/// Double-quoted `"$T"`.
#[test]
fn g02_git_add_double_quoted_var() {
    let fx = fixture("g02");
    let wt = fx.own_worktree();
    parity_at(
        &fx,
        &wt,
        "git add \"crates/x/tests/backlog_x.rs\"",
        "allow",
        "T=crates/x/tests/backlog_x.rs; git add \"$T\"",
    );
}

/// Rows L275/L276 shape: `git add $T && git commit -F - <<'EOF' … EOF`.
#[test]
fn g03_git_add_var_then_commit_heredoc() {
    let fx = fixture("g03");
    let wt = fx.own_worktree();
    parity_at(
        &fx,
        &wt,
        "git add crates/x/tests/backlog_x.rs && git commit -F - <<'EOF'\ntest: add RED\nEOF",
        "allow",
        "T=crates/x/tests/backlog_x.rs; git add $T && git commit -F - <<'EOF'\ntest: add RED\nEOF",
    );
}

/// `git commit -m … -- $T`.
#[test]
fn g04_git_commit_pathspec_var() {
    let fx = fixture("g04");
    let wt = fx.own_worktree();
    parity_at(
        &fx,
        &wt,
        "git commit -m wip -- crates/x/tests/backlog_x.rs",
        "allow",
        "T=crates/x/tests/backlog_x.rs; git commit -m wip -- $T",
    );
}

/// Two literal-bound variables, two explicit pathspecs.
#[test]
fn g05_git_add_two_vars() {
    let fx = fixture("g05");
    let wt = fx.own_worktree();
    parity_at(
        &fx,
        &wt,
        "git add crates/x/Cargo.toml crates/x/src/lib.rs",
        "allow",
        "A=crates/x/Cargo.toml; B=crates/x/src/lib.rs; git add $A $B",
    );
}

/// Rows L16/L62 shape: backslash-newline continuations between explicit
/// pathspecs. The literal spelling is the same argv on one line; a
/// backslash-newline is a line continuation (removed by the shell), not a
/// word.
// test-weakening-justified: new RED for an unfixed FP, observed failing at origin/main 04d9b8336 (2026-11-18)
#[test]
fn g06_git_add_backslash_newline_continuation() {
    let fx = fixture("g06");
    let wt = fx.own_worktree();
    parity_at(
        &fx,
        &wt,
        "git add .claude-plugin/marketplace.json crates/x/.claude-plugin/plugin.json crates/x/Cargo.toml",
        "allow",
        "git add \\\n  .claude-plugin/marketplace.json \\\n  crates/x/.claude-plugin/plugin.json \\\n  crates/x/Cargo.toml",
    );
}

/// Same continuation shape followed by `&& git commit -F - <<'EOF'`.
// test-weakening-justified: new RED for an unfixed FP, observed failing at origin/main 04d9b8336 (2026-11-18)
#[test]
fn g07_git_add_continuation_then_commit_heredoc() {
    let fx = fixture("g07");
    let wt = fx.own_worktree();
    parity_at(
        &fx,
        &wt,
        "git add crates/x/Cargo.toml crates/x/src/lib.rs && git commit -F - <<'EOF'\nx: bump\nEOF",
        "allow",
        "git add \\\n  crates/x/Cargo.toml \\\n  crates/x/src/lib.rs && git commit -F - <<'EOF'\nx: bump\nEOF",
    );
}

/// Row L142 shape: a glob bounded by a literal directory prefix
/// (`crates/x/tests/backlog_20261114_*.rs`). No parity spelling exists (the
/// literal IS the glob), so the expectation is stated directly: every path
/// it can match lies under `crates/x/tests/` (git's pathspec glob anchors the
/// literal prefix; the shell expands it to explicit paths when it matches),
/// so it is not whole-tree staging (B.4 GitStage target = each pathspec,
/// Table C OwnWorktree x GitStage = Allow). Source of the expectation: the
/// replay note "bounded glob/explicit pathspec is not whole-tree staging"
/// and the ruling's "literal-dir globs in non-command-word positions". The
/// git_stage.rs module doc today calls every glob pathspec whole-tree; that
/// contradiction is for the implementer to resolve with a ruling if needed.
// test-weakening-justified: new RED for an unfixed FP, observed failing at origin/main 04d9b8336 (2026-11-18)
#[test]
fn g08_git_add_bounded_glob_allows() {
    let fx = fixture("g08");
    let wt = fx.own_worktree();
    allow_at(&fx, &wt, "git add crates/x/tests/backlog_20261114_*.rs");
}

/// Row L16 shape: `git add -- \<newline> <paths…> && git commit -m
/// "$(cat <<'EOF' … EOF)"`.
// test-weakening-justified: new RED for an unfixed FP, observed failing at origin/main 04d9b8336 (2026-11-18)
#[test]
fn g09_git_add_dashdash_continuation_then_commit_substitution() {
    let fx = fixture("g09");
    let wt = fx.own_worktree();
    parity_at(
        &fx,
        &wt,
        "git add -- crates/x/Cargo.toml crates/x/src/lib.rs && git commit -m \"$(cat <<'EOF'\nfix: x\nEOF\n)\"",
        "allow",
        "git add -- \\\n  crates/x/Cargo.toml \\\n  crates/x/src/lib.rs && \\\ngit commit -m \"$(cat <<'EOF'\nfix: x\nEOF\n)\"",
    );
}

/// Rows L264/L266 shape: a bounded glob next to explicit paths, then
/// `git commit -q -F - <<'EOF' … | grep`. Direct expectation (see g08).
// test-weakening-justified: new RED for an unfixed FP, observed failing at origin/main 04d9b8336 (2026-11-18)
#[test]
fn g10_git_add_bounded_glob_and_paths_then_commit_allows() {
    let fx = fixture("g10");
    let wt = fx.own_worktree();
    allow_at(
        &fx,
        &wt,
        "git add crates/x/tests/backlog_20261115_*.rs crates/x/Cargo.toml && git commit -q -F - <<'EOF' 2>&1 | grep -E 'BLOCK' | head\nx: RED\nEOF",
    );
}

/// Rows L275/L276 shape: `T=<path>; git add $T && git commit -F - <<EOF`
/// with an UNQUOTED delimiter whose body expands only `$T` (literal-bound),
/// so the body is determined. Literal spelling: the same unquoted
/// here-document with the path written out (no expansion in the body).
#[test]
fn g11_git_commit_unquoted_heredoc_body_expands_literal_var() {
    let fx = fixture("g11");
    let wt = fx.own_worktree();
    parity_at(
        &fx,
        &wt,
        "git add crates/x/tests/backlog_x.rs && git commit -q -F - <<EOF\nx: RED\n\ntest-weakening-justified: crates/x/tests/backlog_x.rs:ignore-added - new RED\nEOF",
        "allow",
        "T=crates/x/tests/backlog_x.rs; git add $T && git commit -q -F - <<EOF\nx: RED\n\ntest-weakening-justified: $T:ignore-added - new RED\nEOF",
    );
}

// ---------------------------------------------------------------------------
// G controls — must stay NOT Allow
// ---------------------------------------------------------------------------

/// `T=.`: the value is whole-tree.
#[test]
fn gc1_git_add_var_dot_restricts() {
    let fx = fixture("gc1");
    let wt = fx.own_worktree();
    restrict_at(&fx, &wt, "T=.; git add $T");
}

/// `T=-A`: the value is the whole-tree flag.
#[test]
fn gc2_git_add_var_dash_a_restricts() {
    let fx = fixture("gc2");
    let wt = fx.own_worktree();
    restrict_at(&fx, &wt, "T=-A; git add $T");
}

/// `T='*'`: an unbounded glob.
#[test]
fn gc3_git_add_var_unbounded_glob_restricts() {
    let fx = fixture("gc3");
    let wt = fx.own_worktree();
    restrict_at(&fx, &wt, "T='*'; git add $T");
}

/// The value exists only at run time.
#[test]
fn gc4_git_add_value_from_command_substitution_restricts() {
    let fx = fixture("gc4");
    let wt = fx.own_worktree();
    restrict_at(&fx, &wt, "T=$(git diff --name-only); git add $T");
}

/// The value is reassigned to `.` inside an `if` body.
#[test]
fn gc5_git_add_conditionally_reassigned_restricts() {
    let fx = fixture("gc5");
    let wt = fx.own_worktree();
    restrict_at(
        &fx,
        &wt,
        "T=crates/x/src/lib.rs; if test -e /tmp/bg-sr-sentinel-flag; then T=.; fi; git add $T",
    );
}
