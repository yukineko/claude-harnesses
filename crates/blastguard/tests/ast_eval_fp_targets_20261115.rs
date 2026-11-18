#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Binary-level FP targets for the `detect::ast` static evaluator
//! (run-20261115-212659-5759, task `ast-tests`), written by an independent
//! test author (not the implementer).
//!
//! Source: `~/.blastguard/blocks/blocks.jsonl` lines 1..=337, labelled in
//! `~/.blastguard/fp-measure/labels-20261115.jsonl` as `fp` with `fp_type`
//! `path-resolution` or `other`, and still `deny`/`ask` in the replay
//! `replay-4d1d74f6c-20261115.jsonl` (94 lines). Of those, only the shapes
//! whose FP is SHELL-STATIC are kept: variable / `for` / `cd` resolution of a
//! path, a literal stdin program fed to a shell or interpreter, and
//! operand-vs-program for `bash`/`sh`.
//!
//! Layout:
//!
//! * `fp_line_<n>_*` (`#[ignore = "RED: ast-eval line <n>"]`): the recovered
//!   command, with the session scratchpad
//!   (`/private/tmp/claude-<uid>/<project>/<session>/scratchpad`) replaced by
//!   `@SCRATCH@` — a per-test scratch dir under the same temp safe root
//!   (`/private/tmp`, else `/tmp`) — and every worktree / repo path
//!   (`~/src/.harness-worktrees/<wt>`, `~/.condukt/worktrees/<wt>`,
//!   `~/src/harness*`) replaced by `@PROJ@`, the fixture project dir (which is
//!   also the payload `cwd` and `CLAUDE_PROJECT_DIR`). Nothing else in the
//!   command text is changed (heredoc bodies included). Each asserts Allow in
//!   BOTH modes (main = no `agent_id`, subagent = `agent_id` set); main/
//!   subagent twin lines are one test (named in the test doc). Four targets
//!   (69, 112, 164, 284) assert only that the verdict is not caused by the
//!   unresolved word, because their resolved twin is still blocked by a
//!   different rule — the reason is stated at each test.
//! * `sanity_line_<n>_resolved_twin_is_allow` (NOT ignored): for every
//!   Allow target, the same command with the resolution done by hand (literal
//!   variables substituted, `for` loops unrolled, the relative target spelled
//!   absolute, or — for the stdin / operand shapes — what the shell would
//!   really run, spelled on the command line). They are Allow at the rev of
//!   this commit, which shows that an Allow target is blocked by the
//!   resolution alone and not by the fixture (cwd / HOME / scratch place).
//! * `control_*` (NOT ignored): shapes that must stay blocked through the
//!   migration; each pins the exact current `(main, subagent)` pair.
//!
//! At base (580b660ce + this file) every `fp_line_*` FAILS under
//! `-- --ignored`; lines 8 and 9 were dropped because they do NOT fail at base
//! in the not-caused form (see below).
//!
//! EXCLUDED (line: reason):
//!
//! * 8: blocked at base by `git reset --hard` (scratch repo), and the resolved
//!   twin is blocked by the same reason; the path rule is never the reported
//!   cause, so a not-caused assertion already passes at base — dropped.
//! * 9: same, blocked by whole-tree staging (`git add .` in the scratch repo)
//!   in the original and in the resolved twin — dropped.
//! * 16, 62: backslash line continuation inside `git add` read as possible
//!   whole-tree staging — lexing of continuation, not one of the three
//!   classes.
//! * 48: `M=--manifest-path\ $PWD/Cargo.toml;` — an assignment word with an
//!   escaped space misread as a command word; assignment lexing / word
//!   splitting, not path resolution.
//! * 49, 67, 119: `rm -rf $S` is already RESOLVED at base; the block is the
//!   confined-temp recursive-rm Ask (Deny in a subagent), a separate rule.
//! * 111: `env -i VAR=value ... "$bin"` — env's own argv semantics.
//! * 122: write-then-run heuristic on a log file (`echo rc=$? >> real.out`).
//! * 134: `rm .../*.bak` — a glob; Undetermined by the contract.
//! * 123, 137, 167, 270, 278, 285: python-level `open()` over literal paths /
//!   python lists in a python program.
//! * 148: whether `probe.py` runs the file written by `printf` (python-level).
//! * 179-192: `python3 -c` with `exec(input())`, `compile()`, `globals()[..]`,
//!   `__builtins__` — python-level semantics.
//! * 260, 262: python heredoc program writing literal paths; the misread
//!   extent is secondary to python-level `open(..,'w')` analysis.
//! * 279, 325: `python3 -c` programs containing `$n` / `$f` from a literal
//!   loop — python-level template judgement.
//! * 332, 336: python `open(sys.argv[1])` over a literal shell loop.
//! * 335: `python3 /dev/stdin "$PWD/$F" <<'PY'` — python program editing the
//!   file named in sys.argv.
//!
//! DOES NOT PROVE: that the AST evaluator is what turns a target green (any
//! fix that resolves the word would); verdicts for shapes not literally
//! listed; reason texts (except the forbidden spelling of the four
//! not-caused targets); that a not-caused target is not ALSO hitting the path
//! rule behind another rule's reason (the hook reports one reason); that the
//! controls are the complete set of shapes the migration can break; anything
//! about the user's real scratchpad / worktree paths beyond their class
//! (temp safe root / project dir).

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/neutral_base.rs"]
mod neutral_base;

use std::path::PathBuf;
use std::process::{Command, Stdio};

struct Fx {
    base: PathBuf,
    home: PathBuf,
    proj: PathBuf,
    tmp_root: PathBuf,
    scratch: PathBuf,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
        let _ = std::fs::remove_dir_all(&self.tmp_root);
    }
}

/// The temp safe root the recovered scratchpad paths lived under
/// (`/private/tmp/claude-<uid>/...`): `/private/tmp` where it exists
/// (macOS), else `/tmp`.
fn temp_root() -> PathBuf {
    let p = PathBuf::from("/private/tmp");
    if p.is_dir() {
        p
    } else {
        PathBuf::from("/tmp")
    }
}

impl Fx {
    fn new(name: &str) -> Fx {
        let base = neutral_base::neutral_base()
            .join("ast_eval_fp_targets_20261115")
            .join(name);
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        let proj = base.join("src/proj");
        std::fs::create_dir_all(&home).unwrap();
        for d in [
            ".claude-plugin",
            ".backlog",
            "crates/blastguard/.claude-plugin",
            "crates/blastguard/src/detect/interpreter",
            "crates/blastguard/tests",
            "crates/condukt/.claude-plugin",
            "crates/condukt/src",
            "crates/backlog/tests",
            "target/debug",
        ] {
            std::fs::create_dir_all(proj.join(d)).unwrap();
        }
        let tmp_root = temp_root().join(format!(
            "blastguard-ast-eval-fp-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp_root);
        let scratch = tmp_root.join("scratchpad");
        std::fs::create_dir_all(&scratch).unwrap();
        Fx {
            base,
            home,
            proj,
            tmp_root,
            scratch,
        }
    }

    /// `@PROJ@` / `@SCRATCH@` / `@HOME@` in `command` are replaced by the
    /// fixture project dir, scratch dir and HOME.
    fn run(&self, command: &str, subagent: bool) -> (&'static str, String) {
        let command = command
            .replace("@PROJ@", &self.proj.display().to_string())
            .replace("@SCRATCH@", &self.scratch.display().to_string())
            .replace("@HOME@", &self.home.display().to_string());
        let mut payload = serde_json::json!({
            "session_id": "ast-eval-fp-targets-20261115",
            "hook_event_name": "PreToolUse",
            "cwd": self.proj.display().to_string(),
            "tool_name": "Bash",
            "tool_input": { "command": command },
        });
        if subagent {
            payload["agent_id"] = serde_json::json!("test-subagent");
        }
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("CLAUDE_PROJECT_DIR", &self.proj)
            .env("BLASTGUARD_APPROVALS_DIR", self.base.join("store"))
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_ENTRYPOINT", "cli")
            .env("CLAUDECODE", "1")
            .env_remove("BLASTGUARD_ASK")
            .env_remove("TMPDIR")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .current_dir(&self.base);
        let out = bounded::run_bounded(
            &mut cmd,
            payload.to_string().as_bytes(),
            bounded::HOOK_DEADLINE,
        );
        assert_eq!(
            out.status.code(),
            Some(0),
            "hook must exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let s = String::from_utf8_lossy(&out.stdout).into_owned();
        let v = verdict(&s);
        let reason = if v == "allow" {
            String::new()
        } else {
            let doc: serde_json::Value = serde_json::from_str(s.trim()).unwrap();
            doc["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .unwrap_or("")
                .to_string()
        };
        (v, reason)
    }
}

fn verdict(out: &str) -> &'static str {
    if out.trim().is_empty() {
        "allow"
    } else if out.contains(r#""permissionDecision":"deny""#) {
        "deny"
    } else if out.contains(r#""permissionDecision":"ask""#) {
        "ask"
    } else {
        "other"
    }
}

/// Make the fixture project a git work tree with one committed, clean file
/// (`crates/blastguard/src/detect/git.rs`), like the condukt worktree the
/// recovered line 10 ran in: without it `git checkout -- <file>` is an Ask
/// ("git could not be asked") that has nothing to do with path resolution.
fn git_init(dir: &std::path::Path) {
    let g = |args: &[&str]| {
        let st = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("HOME", dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .unwrap();
        assert!(st.success(), "git {args:?} failed in the fixture");
    };
    std::fs::write(dir.join("crates/blastguard/src/detect/git.rs"), "x\n").unwrap();
    g(&["init", "-q"]);
    g(&["add", "--", "crates/blastguard/src/detect/git.rs"]);
    g(&[
        "-c",
        "user.email=a@b",
        "-c",
        "user.name=t",
        "commit",
        "-q",
        "-m",
        "i",
    ]);
}

fn fixture(tag: &str, git: bool) -> Fx {
    let fx = Fx::new(tag);
    if git {
        git_init(&fx.proj);
    }
    fx
}

/// `command` must be Allow in BOTH modes (main = no `agent_id`, subagent =
/// `agent_id` set).
fn assert_allow_both(tag: &str, command: &str, git: bool) {
    let fx = fixture(tag, git);
    let mut bad = Vec::new();
    for subagent in [false, true] {
        let (d, r) = fx.run(command, subagent);
        println!("[{tag}] subagent={subagent} => {d} {r:?}");
        if d != "allow" {
            bad.push(format!("subagent={subagent} => {d} ({r})"));
        }
    }
    assert!(
        bad.is_empty(),
        "{tag}: expected Allow in both modes:\n{}",
        bad.join("\n")
    );
}

/// Re-anchor target of user ruling 2026-11-18 (S4 B.5): the command runs a
/// program file (`python3 x.py`, a direct project-path exec), which is
/// Undetermined — main Ask, subagent Deny. The main reason must be the B.5
/// layer's: B.5 never replaces a legacy Ask's reason (equal rank keeps
/// legacy), so a B.5 reason shows the legacy readings — the FP this file
/// targets — answered Allow.
fn assert_b5_undetermined(tag: &str, command: &str, git: bool) {
    let fx = fixture(tag, git);
    let (dm, rm) = fx.run(command, false);
    println!("[{tag}] subagent=false => {dm} {rm:?}");
    assert!(
        dm == "ask" && rm.contains("Table B.5"),
        "{tag}: expected the B.5 Ask in main mode, got {dm} ({rm})"
    );
    let (ds, rs) = fx.run(command, true);
    println!("[{tag}] subagent=true => {ds} {rs:?}");
    assert_eq!(ds, "deny", "{tag}: subagent expected Deny ({rs})");
}

/// In both modes the verdict must be Allow, or its reason must not name the
/// unresolved word `spelling` (i.e. the verdict is not caused by the
/// path / stdin rule this file targets). The hook reports ONE reason, so this
/// cannot see a path-rule finding hidden behind another rule's reason.
fn assert_not_caused_by(tag: &str, command: &str, spelling: &str) {
    let fx = fixture(tag, false);
    let mut bad = Vec::new();
    for subagent in [false, true] {
        let (d, r) = fx.run(command, subagent);
        println!("[{tag}] subagent={subagent} => {d} {r:?}");
        if d != "allow" && r.contains(spelling) {
            bad.push(format!("subagent={subagent} => {d} ({r})"));
        }
    }
    assert!(
        bad.is_empty(),
        "{tag}: verdict still caused by the unresolved word {spelling:?}:\n{}",
        bad.join("\n")
    );
}

/// Control: the exact current `(main, subagent)` pair, which must be a
/// restrictive decision in both modes.
fn check_control(tag: &str, command: &str, main: &str, sub: &str) {
    assert!(
        matches!(main, "deny" | "ask") && matches!(sub, "deny" | "ask"),
        "a control must expect Deny/Ask: {main}/{sub}"
    );
    let fx = Fx::new(tag);
    let (m, mr) = fx.run(command, false);
    let (s, sr) = fx.run(command, true);
    println!("[{tag}] main => {m} {mr:?}\n[{tag}] sub => {s} {sr:?}");
    assert_eq!(
        (m, s),
        (main, sub),
        "control {tag} (main, subagent); cmd={command:?}\n main reason={mr}\n sub reason={sr}"
    );
}

// ===================================================================
// COMMANDS (recovered from blocks.jsonl; scratchpad / worktree paths
// replaced by fixture placeholders, nothing else changed)
// ===================================================================

/// blocks.jsonl line 2 (2=subagent), fixture-substituted.
const L2: &str = r####"cd @PROJ@ && D=@SCRATCH@/condukt; condukt validate --file $D/decomp.json; echo "validate=$?"; command -v schemaguard && { schemaguard check --schema decomposition --file $D/decomp.json; echo "sg=$?"; }; if command -v fugu-router >/dev/null; then fugu-router route --file $D/decomp.json --report $D/route.json > $D/decomp.routed.json; echo "route=$?"; else cp $D/decomp.json $D/decomp.routed.json; fi; condukt schedule --file $D/decomp.routed.json; echo "sched=$?"; cat $D/route.json 2>/dev/null | head -40"####;
/// Allow-twin of line 2: the same command with the var resolution done by hand.
const T2: &str = r####"cd @PROJ@ && D=@SCRATCH@/condukt; condukt validate --file @SCRATCH@/condukt/decomp.json; echo "validate=$?"; command -v schemaguard && { schemaguard check --schema decomposition --file @SCRATCH@/condukt/decomp.json; echo "sg=$?"; }; if command -v fugu-router >/dev/null; then fugu-router route --file @SCRATCH@/condukt/decomp.json --report @SCRATCH@/condukt/route.json > @SCRATCH@/condukt/decomp.routed.json; echo "route=$?"; else cp @SCRATCH@/condukt/decomp.json @SCRATCH@/condukt/decomp.routed.json; fi; condukt schedule --file @SCRATCH@/condukt/decomp.routed.json; echo "sched=$?"; cat @SCRATCH@/condukt/route.json 2>/dev/null | head -40"####;
/// blocks.jsonl line 7 (7=subagent), fixture-substituted.
const L7: &str = r####"S=@SCRATCH@/condukt; . "$HOME/.cargo/env"; cd @PROJ@ && cargo test -p blastguard --test heredoc_script_failopen_red_20261108 heredoc_script_attacks_are_not_allow 2>&1 | grep -E "main=" | sort > $S/heredoc_base.txt; cd @PROJ@ && cargo test -p blastguard --test heredoc_script_failopen_red_20261108 heredoc_script_attacks_are_not_allow 2>&1 | grep -E "main=" | sort > $S/heredoc_B.txt; wc -l $S/heredoc_base.txt $S/heredoc_B.txt; diff $S/heredoc_base.txt $S/heredoc_B.txt && echo IDENTICAL"####;
/// Allow-twin of line 7: the same command with the var resolution done by hand.
const T7: &str = r####"S=@SCRATCH@/condukt; . "$HOME/.cargo/env"; cd @PROJ@ && cargo test -p blastguard --test heredoc_script_failopen_red_20261108 heredoc_script_attacks_are_not_allow 2>&1 | grep -E "main=" | sort > @SCRATCH@/condukt/heredoc_base.txt; cd @PROJ@ && cargo test -p blastguard --test heredoc_script_failopen_red_20261108 heredoc_script_attacks_are_not_allow 2>&1 | grep -E "main=" | sort > @SCRATCH@/condukt/heredoc_B.txt; wc -l @SCRATCH@/condukt/heredoc_base.txt @SCRATCH@/condukt/heredoc_B.txt; diff @SCRATCH@/condukt/heredoc_base.txt @SCRATCH@/condukt/heredoc_B.txt && echo IDENTICAL"####;
/// blocks.jsonl line 10 (10=subagent), fixture-substituted.
const L10: &str = r####"cd @PROJ@ && . "$HOME/.cargo/env" && S=@SCRATCH@ && mkdir -p $S && git diff -- crates/blastguard/src/detect/git.rs > $S/fix.patch && git checkout -- crates/blastguard/src/detect/git.rs && cargo test -q -p blastguard --test reset_hard_clean_worktree_9352d929 2>&1 | tail -25; git apply $S/fix.patch && git diff --stat"####;
/// Allow-twin of line 10: the same command with the var resolution done by hand.
const T10: &str = r####"cd @PROJ@ && . "$HOME/.cargo/env" && S=@SCRATCH@ && mkdir -p @SCRATCH@ && git diff -- crates/blastguard/src/detect/git.rs > @SCRATCH@/fix.patch && git checkout -- crates/blastguard/src/detect/git.rs && cargo test -q -p blastguard --test reset_hard_clean_worktree_9352d929 2>&1 | tail -25; git apply @SCRATCH@/fix.patch && git diff --stat"####;
/// blocks.jsonl line 34 (34=subagent), fixture-substituted.
const L34: &str = r####"cd @PROJ@ && B=@SCRATCH@/f70e-bak && mkdir -p $B && cp crates/blastguard/src/scope.rs crates/blastguard/src/detect/rm.rs $B/ && grep -n 'self.resolve(&prefix).as_deref() != Some(prefix.as_str())\|if !self.resolves_to_existing_dir(&prefix)$\|return Determination::undetermined("`..` climbs above `/`");\|                    comps.pop();' crates/blastguard/src/scope.rs"####;
/// Allow-twin of line 34: the same command with the var resolution done by hand.
const T34: &str = r####"cd @PROJ@ && B=@SCRATCH@/f70e-bak && mkdir -p @SCRATCH@/f70e-bak && cp crates/blastguard/src/scope.rs crates/blastguard/src/detect/rm.rs @SCRATCH@/f70e-bak/ && grep -n 'self.resolve(&prefix).as_deref() != Some(prefix.as_str())\|if !self.resolves_to_existing_dir(&prefix)$\|return Determination::undetermined("`..` climbs above `/`");\|                    comps.pop();' crates/blastguard/src/scope.rs"####;
/// blocks.jsonl line 35 (35=subagent), fixture-substituted.
const L35: &str = r####"cd @PROJ@ && . "$HOME/.cargo/env" && M=@PROJ@/Cargo.toml; S=@SCRATCH@; for t in backlog_20261114_e787b168_sync_remote_undetermined cli_contract backlog_7e575392 close_records_content; do cargo test --manifest-path $M -p backlog --test $t > $S/$t.txt 2>&1; echo "$t exit=$?"; grep "test result" $S/$t.txt; done; cargo test --manifest-path $M -p backlog --lib sync > $S/lib.txt 2>&1; echo "lib sync exit=$?"; grep "test result" $S/lib.txt"####;
/// Allow-twin of line 35: the same command with the for resolution done by hand.
const T35: &str = r####"cd @PROJ@ && . "$HOME/.cargo/env" && M=@PROJ@/Cargo.toml; S=@SCRATCH@; cargo test --manifest-path @PROJ@/Cargo.toml -p backlog --test backlog_20261114_e787b168_sync_remote_undetermined > @SCRATCH@/backlog_20261114_e787b168_sync_remote_undetermined.txt 2>&1; echo "backlog_20261114_e787b168_sync_remote_undetermined exit=$?"; grep "test result" @SCRATCH@/backlog_20261114_e787b168_sync_remote_undetermined.txt; cargo test --manifest-path @PROJ@/Cargo.toml -p backlog --test cli_contract > @SCRATCH@/cli_contract.txt 2>&1; echo "cli_contract exit=$?"; grep "test result" @SCRATCH@/cli_contract.txt; cargo test --manifest-path @PROJ@/Cargo.toml -p backlog --test backlog_7e575392 > @SCRATCH@/backlog_7e575392.txt 2>&1; echo "backlog_7e575392 exit=$?"; grep "test result" @SCRATCH@/backlog_7e575392.txt; cargo test --manifest-path @PROJ@/Cargo.toml -p backlog --test close_records_content > @SCRATCH@/close_records_content.txt 2>&1; echo "close_records_content exit=$?"; grep "test result" @SCRATCH@/close_records_content.txt; cargo test --manifest-path @PROJ@/Cargo.toml -p backlog --lib sync > @SCRATCH@/lib.txt 2>&1; echo "lib sync exit=$?"; grep "test result" @SCRATCH@/lib.txt"####;
/// blocks.jsonl line 52 (52=interactive), fixture-substituted.
const L52: &str = r####"cd @PROJ@ && git log --oneline -1 && S=@SCRATCH@ && backlog list --json --status all > $S/bl.json 2>/dev/null || backlog list --json > $S/bl.json; jq -r '.[] | select(.id=="596ef57d" or .id=="070b696c" or .id=="c96e756a") | "\(.id) \(.status) notes_len=\(.notes|length)"' $S/bl.json"####;
/// Allow-twin of line 52: the same command with the var resolution done by hand.
const T52: &str = r####"cd @PROJ@ && git log --oneline -1 && S=@SCRATCH@ && backlog list --json --status all > @SCRATCH@/bl.json 2>/dev/null || backlog list --json > @SCRATCH@/bl.json; jq -r '.[] | select(.id=="596ef57d" or .id=="070b696c" or .id=="c96e756a") | "\(.id) \(.status) notes_len=\(.notes|length)"' @SCRATCH@/bl.json"####;
/// blocks.jsonl line 53 (53=subagent, 54=interactive), fixture-substituted.
const L53: &str = r####"cd @PROJ@/crates/backlog && cat > tests/blastguard_classification_tag_required_20261114.rs <<'EOF'
//! User ruling 2026-11-14: every new blastguard backlog item must carry a
//! classification tag (`fail-open` or `false-positive`) so the weekly metric
//! `scripts/blastguard-discovery-rate.py` stops showing ~40% unclassified.
//!
//! A task is "blastguard" if its tags contain `blastguard` OR its title starts
//! with `blastguard` (case-insensitive) — the metric script's selection.
//!
//! Proves (driving the real binary in a hermetic fixture):
//!   * `backlog add` of a blastguard task with neither tag is refused:
//!     non-zero exit, stderr names both accepted tags, store bytes unchanged,
//!     and the repro command is NOT executed (marker file never created).
//!   * `backlog edit --tag ...` / `--title ...` that would leave a blastguard
//!     task without either tag is refused with the store unchanged.
//!   * Controls (pass today, must keep passing): tagged add succeeds (either
//!     tag or both), non-blastguard add needs no tag, notes-only edit of a
//!     legacy untagged blastguard row succeeds, edit that supplies a tag works.
//!
//! Does NOT prove: that no GitHub issue is attempted (the fixture has no gh;
//! only the store-unchanged + no-repro-run observations are asserted), nor the
//! exact stderr wording beyond naming both tags, nor behaviour of `sync`/other
//! entry points that might create rows.
mod common;
use common::*;

const TAGS: [&str; 2] = ["fail-open", "false-positive"];

/// Repro script that leaves a marker when executed; exit 1 = reproduced.
fn fixture(tag: &str) -> Fixture {
    let f = Fixture::new(tag);
    f.write_script("tests/repro_marker.sh", "touch repro_ran.marker; exit 1");
    f.commit_paths(&["tests/repro_marker.sh"], "marker repro");
    f
}

fn add(f: &Fixture, title: &str, tags: &[&str]) -> (Out, Option<String>) {
    let mut extra: Vec<&str> = vec!["--repro-test", "bash tests/repro_marker.sh"];
    for t in tags {
        extra.push("--tag");
        extra.push(t);
    }
    f.add_extra(title, &extra)
}

fn assert_refused_naming_tags(f: &Fixture, before: &[u8], o: &Out, why: &str) {
    assert_ne!(o.code, 0, "{why}: must be refused: {}", o.both());
    for t in TAGS {
        assert!(o.stderr.contains(t), "{why}: stderr must name `{t}`: {}", o.both());
    }
    assert_eq!(f.store_bytes(), before, "{why}: store must be unchanged");
}

fn tags_of(f: &Fixture, id: &str) -> Vec<String> {
    f.row(id)["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

// ---------- add: refusals (RED today) ----------

#[test]
fn add_blastguard_tag_without_classification_is_refused() {
    let f = fixture("bgc-add-tag");
    let before = f.store_bytes();
    let (o, _) = add(&f, "some gate problem", &["blastguard"]);
    assert_refused_naming_tags(&f, &before, &o, "tag blastguard, no classification");
    assert!(
        !f.repo.join("repro_ran.marker").exists(),
        "repro must not run for a refused add"
    );
}

#[test]
fn add_blastguard_title_without_classification_is_refused() {
    let f = fixture("bgc-add-title");
    let before = f.store_bytes();
    let (o, _) = add(&f, "blastguard: rm -rf wrapper slips through", &[]);
    assert_refused_naming_tags(&f, &before, &o, "blastguard title, no tags");
    assert!(
        !f.repo.join("repro_ran.marker").exists(),
        "repro must not run for a refused add"
    );
}

#[test]
fn add_blastguard_title_is_case_insensitive() {
    let f = fixture("bgc-add-case");
    let before = f.store_bytes();
    let (o, _) = add(&f, "BlastGuard allows X", &["p1"]);
    assert_refused_naming_tags(&f, &before, &o, "mixed-case blastguard title, unrelated tag");
}

#[test]
fn add_blastguard_with_unrelated_tags_only_is_refused() {
    let f = fixture("bgc-add-other");
    let before = f.store_bytes();
    let (o, _) = add(&f, "gate gap", &["blastguard", "p0", "security"]);
    assert_refused_naming_tags(&f, &before, &o, "blastguard + unrelated tags");
}

// ---------- add: controls (pass today) ----------

#[test]
fn control_add_blastguard_with_fail_open_succeeds() {
    let f = fixture("bgc-ok-fo");
    let (o, id) = add(&f, "blastguard: x", &["fail-open"]);
    assert_eq!(o.code, 0, "{}", o.both());
    assert!(tags_of(&f, &id.unwrap()).contains(&"fail-open".to_string()));
}

#[test]
fn control_add_blastguard_with_false_positive_succeeds() {
    let f = fixture("bgc-ok-fp");
    let (o, id) = add(&f, "x", &["blastguard", "false-positive"]);
    assert_eq!(o.code, 0, "{}", o.both());
    assert!(id.is_some());
}

#[test]
fn control_add_blastguard_with_both_tags_succeeds() {
    let f = fixture("bgc-ok-both");
    let (o, id) = add(&f, "blastguard: y", &["fail-open", "false-positive"]);
    assert_eq!(o.code, 0, "{}", o.both());
    assert!(id.is_some());
}

#[test]
fn control_add_non_blastguard_needs_no_tag() {
    let f = fixture("bgc-ok-nonbg");
    let (o, id) = add(&f, "unrelated task about statusline", &[]);
    assert_eq!(o.code, 0, "{}", o.both());
    assert!(id.is_some());
}

// ---------- edit ----------

/// A legacy open blastguard row with no tags, planted straight into the store.
fn legacy_untagged(f: &Fixture, id: &str, title: &str) {
    let (_other, label) = f.add_with_label("seed row for project label");
    f.plant_open(id, title, "pending", &label);
}

#[test]
fn edit_replacing_tags_so_blastguard_row_loses_classification_is_refused() {
    let f = fixture("bgc-edit-drop");
    let (_, id) = add(&f, "blastguard: z", &["fail-open"]);
    let id = id.unwrap();
    let before = f.store_bytes();
    let o = f.run(&["edit", &id, "--tag", "p1"]);
    assert_refused_naming_tags(&f, &before, &o, "edit drops classification");
}

#[test]
fn edit_adding_blastguard_tag_without_classification_is_refused() {
    let f = fixture("bgc-edit-addbg");
    let (_, id) = add(&f, "plain task", &[]);
    let id = id.unwrap();
    let before = f.store_bytes();
    let o = f.run(&["edit", &id, "--tag", "blastguard"]);
    assert_refused_naming_tags(&f, &before, &o, "edit makes row blastguard by tag");
}

#[test]
fn edit_title_to_blastguard_prefix_on_untagged_row_is_refused() {
    let f = fixture("bgc-edit-title");
    let (_, id) = add(&f, "plain task", &[]);
    let id = id.unwrap();
    let before = f.store_bytes();
    let o = f.run(&["edit", &id, "--title", "blastguard: now it is one"]);
    assert_refused_naming_tags(&f, &before, &o, "edit makes row blastguard by title");
}

#[test]
fn edit_legacy_untagged_row_with_unrelated_tag_is_refused() {
    let f = fixture("bgc-edit-legacy-tag");
    legacy_untagged(&f, "legacy01", "blastguard: old row");
    let before = f.store_bytes();
    let o = f.run(&["edit", "legacy01", "--tag", "p2"]);
    assert_refused_naming_tags(&f, &before, &o, "resulting tags lack classification");
}

// ---------- edit: controls (pass today) ----------

#[test]
fn control_notes_only_edit_of_legacy_untagged_blastguard_row_succeeds() {
    let f = fixture("bgc-edit-legacy-notes");
    legacy_untagged(&f, "legacy02", "blastguard: old row");
    let o = f.run(&["edit", "legacy02", "--notes", "triaged"]);
    assert_eq!(o.code, 0, "legacy rows stay editable: {}", o.both());
    assert_eq!(f.row("legacy02")["notes"], "triaged");
}

#[test]
fn control_edit_supplying_classification_on_legacy_row_succeeds() {
    let f = fixture("bgc-edit-legacy-fix");
    legacy_untagged(&f, "legacy03", "blastguard: old row");
    let o = f.run(&["edit", "legacy03", "--tag", "false-positive"]);
    assert_eq!(o.code, 0, "{}", o.both());
    assert!(tags_of(&f, "legacy03").contains(&"false-positive".to_string()));
}

#[test]
fn control_edit_non_blastguard_row_tags_unaffected() {
    let f = fixture("bgc-edit-nonbg");
    let (_, id) = add(&f, "plain task", &[]);
    let o = f.run(&["edit", &id.unwrap(), "--tag", "p2"]);
    assert_eq!(o.code, 0, "{}", o.both());
}
EOF
cd @PROJ@ && . "$HOME/.cargo/env" && cargo test -p backlog --test blastguard_classification_tag_required_20261114 2>&1 | tail -50"####;
/// Allow-twin of line 53: the same command with the cd resolution done by hand.
const T53: &str = r####"cd @PROJ@/crates/backlog && cat > @PROJ@/crates/backlog/tests/blastguard_classification_tag_required_20261114.rs <<'EOF'
//! User ruling 2026-11-14: every new blastguard backlog item must carry a
//! classification tag (`fail-open` or `false-positive`) so the weekly metric
//! `scripts/blastguard-discovery-rate.py` stops showing ~40% unclassified.
//!
//! A task is "blastguard" if its tags contain `blastguard` OR its title starts
//! with `blastguard` (case-insensitive) — the metric script's selection.
//!
//! Proves (driving the real binary in a hermetic fixture):
//!   * `backlog add` of a blastguard task with neither tag is refused:
//!     non-zero exit, stderr names both accepted tags, store bytes unchanged,
//!     and the repro command is NOT executed (marker file never created).
//!   * `backlog edit --tag ...` / `--title ...` that would leave a blastguard
//!     task without either tag is refused with the store unchanged.
//!   * Controls (pass today, must keep passing): tagged add succeeds (either
//!     tag or both), non-blastguard add needs no tag, notes-only edit of a
//!     legacy untagged blastguard row succeeds, edit that supplies a tag works.
//!
//! Does NOT prove: that no GitHub issue is attempted (the fixture has no gh;
//! only the store-unchanged + no-repro-run observations are asserted), nor the
//! exact stderr wording beyond naming both tags, nor behaviour of `sync`/other
//! entry points that might create rows.
mod common;
use common::*;

const TAGS: [&str; 2] = ["fail-open", "false-positive"];

/// Repro script that leaves a marker when executed; exit 1 = reproduced.
fn fixture(tag: &str) -> Fixture {
    let f = Fixture::new(tag);
    f.write_script("tests/repro_marker.sh", "touch repro_ran.marker; exit 1");
    f.commit_paths(&["tests/repro_marker.sh"], "marker repro");
    f
}

fn add(f: &Fixture, title: &str, tags: &[&str]) -> (Out, Option<String>) {
    let mut extra: Vec<&str> = vec!["--repro-test", "bash tests/repro_marker.sh"];
    for t in tags {
        extra.push("--tag");
        extra.push(t);
    }
    f.add_extra(title, &extra)
}

fn assert_refused_naming_tags(f: &Fixture, before: &[u8], o: &Out, why: &str) {
    assert_ne!(o.code, 0, "{why}: must be refused: {}", o.both());
    for t in TAGS {
        assert!(o.stderr.contains(t), "{why}: stderr must name `{t}`: {}", o.both());
    }
    assert_eq!(f.store_bytes(), before, "{why}: store must be unchanged");
}

fn tags_of(f: &Fixture, id: &str) -> Vec<String> {
    f.row(id)["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

// ---------- add: refusals (RED today) ----------

#[test]
fn add_blastguard_tag_without_classification_is_refused() {
    let f = fixture("bgc-add-tag");
    let before = f.store_bytes();
    let (o, _) = add(&f, "some gate problem", &["blastguard"]);
    assert_refused_naming_tags(&f, &before, &o, "tag blastguard, no classification");
    assert!(
        !f.repo.join("repro_ran.marker").exists(),
        "repro must not run for a refused add"
    );
}

#[test]
fn add_blastguard_title_without_classification_is_refused() {
    let f = fixture("bgc-add-title");
    let before = f.store_bytes();
    let (o, _) = add(&f, "blastguard: rm -rf wrapper slips through", &[]);
    assert_refused_naming_tags(&f, &before, &o, "blastguard title, no tags");
    assert!(
        !f.repo.join("repro_ran.marker").exists(),
        "repro must not run for a refused add"
    );
}

#[test]
fn add_blastguard_title_is_case_insensitive() {
    let f = fixture("bgc-add-case");
    let before = f.store_bytes();
    let (o, _) = add(&f, "BlastGuard allows X", &["p1"]);
    assert_refused_naming_tags(&f, &before, &o, "mixed-case blastguard title, unrelated tag");
}

#[test]
fn add_blastguard_with_unrelated_tags_only_is_refused() {
    let f = fixture("bgc-add-other");
    let before = f.store_bytes();
    let (o, _) = add(&f, "gate gap", &["blastguard", "p0", "security"]);
    assert_refused_naming_tags(&f, &before, &o, "blastguard + unrelated tags");
}

// ---------- add: controls (pass today) ----------

#[test]
fn control_add_blastguard_with_fail_open_succeeds() {
    let f = fixture("bgc-ok-fo");
    let (o, id) = add(&f, "blastguard: x", &["fail-open"]);
    assert_eq!(o.code, 0, "{}", o.both());
    assert!(tags_of(&f, &id.unwrap()).contains(&"fail-open".to_string()));
}

#[test]
fn control_add_blastguard_with_false_positive_succeeds() {
    let f = fixture("bgc-ok-fp");
    let (o, id) = add(&f, "x", &["blastguard", "false-positive"]);
    assert_eq!(o.code, 0, "{}", o.both());
    assert!(id.is_some());
}

#[test]
fn control_add_blastguard_with_both_tags_succeeds() {
    let f = fixture("bgc-ok-both");
    let (o, id) = add(&f, "blastguard: y", &["fail-open", "false-positive"]);
    assert_eq!(o.code, 0, "{}", o.both());
    assert!(id.is_some());
}

#[test]
fn control_add_non_blastguard_needs_no_tag() {
    let f = fixture("bgc-ok-nonbg");
    let (o, id) = add(&f, "unrelated task about statusline", &[]);
    assert_eq!(o.code, 0, "{}", o.both());
    assert!(id.is_some());
}

// ---------- edit ----------

/// A legacy open blastguard row with no tags, planted straight into the store.
fn legacy_untagged(f: &Fixture, id: &str, title: &str) {
    let (_other, label) = f.add_with_label("seed row for project label");
    f.plant_open(id, title, "pending", &label);
}

#[test]
fn edit_replacing_tags_so_blastguard_row_loses_classification_is_refused() {
    let f = fixture("bgc-edit-drop");
    let (_, id) = add(&f, "blastguard: z", &["fail-open"]);
    let id = id.unwrap();
    let before = f.store_bytes();
    let o = f.run(&["edit", &id, "--tag", "p1"]);
    assert_refused_naming_tags(&f, &before, &o, "edit drops classification");
}

#[test]
fn edit_adding_blastguard_tag_without_classification_is_refused() {
    let f = fixture("bgc-edit-addbg");
    let (_, id) = add(&f, "plain task", &[]);
    let id = id.unwrap();
    let before = f.store_bytes();
    let o = f.run(&["edit", &id, "--tag", "blastguard"]);
    assert_refused_naming_tags(&f, &before, &o, "edit makes row blastguard by tag");
}

#[test]
fn edit_title_to_blastguard_prefix_on_untagged_row_is_refused() {
    let f = fixture("bgc-edit-title");
    let (_, id) = add(&f, "plain task", &[]);
    let id = id.unwrap();
    let before = f.store_bytes();
    let o = f.run(&["edit", &id, "--title", "blastguard: now it is one"]);
    assert_refused_naming_tags(&f, &before, &o, "edit makes row blastguard by title");
}

#[test]
fn edit_legacy_untagged_row_with_unrelated_tag_is_refused() {
    let f = fixture("bgc-edit-legacy-tag");
    legacy_untagged(&f, "legacy01", "blastguard: old row");
    let before = f.store_bytes();
    let o = f.run(&["edit", "legacy01", "--tag", "p2"]);
    assert_refused_naming_tags(&f, &before, &o, "resulting tags lack classification");
}

// ---------- edit: controls (pass today) ----------

#[test]
fn control_notes_only_edit_of_legacy_untagged_blastguard_row_succeeds() {
    let f = fixture("bgc-edit-legacy-notes");
    legacy_untagged(&f, "legacy02", "blastguard: old row");
    let o = f.run(&["edit", "legacy02", "--notes", "triaged"]);
    assert_eq!(o.code, 0, "legacy rows stay editable: {}", o.both());
    assert_eq!(f.row("legacy02")["notes"], "triaged");
}

#[test]
fn control_edit_supplying_classification_on_legacy_row_succeeds() {
    let f = fixture("bgc-edit-legacy-fix");
    legacy_untagged(&f, "legacy03", "blastguard: old row");
    let o = f.run(&["edit", "legacy03", "--tag", "false-positive"]);
    assert_eq!(o.code, 0, "{}", o.both());
    assert!(tags_of(&f, "legacy03").contains(&"false-positive".to_string()));
}

#[test]
fn control_edit_non_blastguard_row_tags_unaffected() {
    let f = fixture("bgc-edit-nonbg");
    let (_, id) = add(&f, "plain task", &[]);
    let o = f.run(&["edit", &id.unwrap(), "--tag", "p2"]);
    assert_eq!(o.code, 0, "{}", o.both());
}
EOF
cd @PROJ@ && . "$HOME/.cargo/env" && cargo test -p backlog --test blastguard_classification_tag_required_20261114 2>&1 | tail -50"####;
/// blocks.jsonl line 57 (57=subagent), fixture-substituted.
const L57: &str = r####"cd @PROJ@ && f=crates/condukt/src/testscope.rs && sed -i '' -e '238s/-> Judged {/-> Result<Option<Words>, String> {/' -e '250s/return Judged::Other,/return Ok(None),/' -e '258s/return Judged::Deny(format!(/return Err(format!(/' -e '267s/return Judged::Deny(format!(/return Err(format!(/' -e '275s/Judged::Test(norm)/Ok(Some(norm))/' $f && sed -n 236,277p $f"####;
/// Allow-twin of line 57: the same command with the var resolution done by hand.
const T57: &str = r####"cd @PROJ@ && f=crates/condukt/src/testscope.rs && sed -i '' -e '238s/-> Judged {/-> Result<Option<Words>, String> {/' -e '250s/return Judged::Other,/return Ok(None),/' -e '258s/return Judged::Deny(format!(/return Err(format!(/' -e '267s/return Judged::Deny(format!(/return Err(format!(/' -e '275s/Judged::Test(norm)/Ok(Some(norm))/' crates/condukt/src/testscope.rs && sed -n 236,277p crates/condukt/src/testscope.rs"####;
/// blocks.jsonl line 59 (59=subagent), fixture-substituted.
const L59: &str = r####"cd @PROJ@ && D=@SCRATCH@/run; condukt validate --file $D/decomp.json; echo v=$?; command -v schemaguard && { schemaguard check --schema decomposition --file $D/decomp.json; echo s=$?; }; if command -v fugu-router >/dev/null; then fugu-router route --file $D/decomp.json --report $D/route.json > $D/decomp.routed.json; echo r=$?; else cp $D/decomp.json $D/decomp.routed.json; fi; condukt schedule --file $D/decomp.routed.json; echo sch=$?; jq -r '.tasks[]|"\(.id) \(.suggested_model)"' $D/decomp.routed.json"####;
/// Allow-twin of line 59: the same command with the var resolution done by hand.
const T59: &str = r####"cd @PROJ@ && D=@SCRATCH@/run; condukt validate --file @SCRATCH@/run/decomp.json; echo v=$?; command -v schemaguard && { schemaguard check --schema decomposition --file @SCRATCH@/run/decomp.json; echo s=$?; }; if command -v fugu-router >/dev/null; then fugu-router route --file @SCRATCH@/run/decomp.json --report @SCRATCH@/run/route.json > @SCRATCH@/run/decomp.routed.json; echo r=$?; else cp @SCRATCH@/run/decomp.json @SCRATCH@/run/decomp.routed.json; fi; condukt schedule --file @SCRATCH@/run/decomp.routed.json; echo sch=$?; jq -r '.tasks[]|"\(.id) \(.suggested_model)"' @SCRATCH@/run/decomp.routed.json"####;
/// blocks.jsonl line 66 (66=subagent), fixture-substituted.
const L66: &str = r####"cd @PROJ@ && . "$HOME/.cargo/env" && O=@SCRATCH@/out.txt; cargo test -p blastguard --test backlog_20261113_fp_heredoc_rs_body_and_commit_msg_quotes --test backlog_20261113_heredoc_body_fp --test backlog_20261113_python_heredoc_body_append_backtick_fp --test backlog_20261114_git_commit_msg_quoted_heredoc_backtick_fp --test backlog_20261114_heredoc_rs_body_match_arrow_fp --test backlog_20261114_json_heredoc_body_sh_heredoc_text_fp --test backlog_20261114_python_heredoc_backtick_string_write_target_fp --test backlog_20261114_quoted_heredoc_body_git_commit_line_after_first_statement_fp --test backlog_20261114_quoted_heredoc_body_side_fps --test backlog_20261114_quoted_heredoc_substitution_text_fp --test backlog_heredoc_data_body_fp_20261105 --test commit_msg_heredoc_substitution_20261107 --test commit_msg_heredoc_trailing_attack_20261108 --test commit_msg_heredoc_trailing_cmd_fp_20261107 --test heredoc_body_gt_fp_20261107 --test heredoc_body_not_redirect --test heredoc_chain_heads --test heredoc_data_body_extraction --test heredoc_data_body_verify --test heredoc_data_body_verify2 --test heredoc_opener_misparse_fp_20261103 --test heredoc_quoted_body_doc_comment_backtick --test heredoc_quoted_body_redirect_false_positive --test heredoc_quoted_body_restrictive_shapes --test heredoc_safe_reader_body --test heredoc_safe_reader_line_continuation --test heredoc_script_failopen_red_20261108 --test heredoc_system_harm_holes_20261111 --test quoted_heredoc_backtick_fp_20261108 --test quoted_heredoc_body_backtick_fp_20261113 --test quoted_heredoc_body_string_parsed_as_command_fp_20261113 --test quoted_heredoc_multiline_backtick_fp_20261114 --test quoted_heredoc_python_gt_fp_20261107 --test subagent_heredoc_fp_20261108 > $O 2>&1; echo exit=$?; grep -E "^test result|FAILED|panicked|^error" $O | sort | uniq -c | head -30"####;
/// Allow-twin of line 66: the same command with the var resolution done by hand.
const T66: &str = r####"cd @PROJ@ && . "$HOME/.cargo/env" && O=@SCRATCH@/out.txt; cargo test -p blastguard --test backlog_20261113_fp_heredoc_rs_body_and_commit_msg_quotes --test backlog_20261113_heredoc_body_fp --test backlog_20261113_python_heredoc_body_append_backtick_fp --test backlog_20261114_git_commit_msg_quoted_heredoc_backtick_fp --test backlog_20261114_heredoc_rs_body_match_arrow_fp --test backlog_20261114_json_heredoc_body_sh_heredoc_text_fp --test backlog_20261114_python_heredoc_backtick_string_write_target_fp --test backlog_20261114_quoted_heredoc_body_git_commit_line_after_first_statement_fp --test backlog_20261114_quoted_heredoc_body_side_fps --test backlog_20261114_quoted_heredoc_substitution_text_fp --test backlog_heredoc_data_body_fp_20261105 --test commit_msg_heredoc_substitution_20261107 --test commit_msg_heredoc_trailing_attack_20261108 --test commit_msg_heredoc_trailing_cmd_fp_20261107 --test heredoc_body_gt_fp_20261107 --test heredoc_body_not_redirect --test heredoc_chain_heads --test heredoc_data_body_extraction --test heredoc_data_body_verify --test heredoc_data_body_verify2 --test heredoc_opener_misparse_fp_20261103 --test heredoc_quoted_body_doc_comment_backtick --test heredoc_quoted_body_redirect_false_positive --test heredoc_quoted_body_restrictive_shapes --test heredoc_safe_reader_body --test heredoc_safe_reader_line_continuation --test heredoc_script_failopen_red_20261108 --test heredoc_system_harm_holes_20261111 --test quoted_heredoc_backtick_fp_20261108 --test quoted_heredoc_body_backtick_fp_20261113 --test quoted_heredoc_body_string_parsed_as_command_fp_20261113 --test quoted_heredoc_multiline_backtick_fp_20261114 --test quoted_heredoc_python_gt_fp_20261107 --test subagent_heredoc_fp_20261108 > @SCRATCH@/out.txt 2>&1; echo exit=$?; grep -E "^test result|FAILED|panicked|^error" @SCRATCH@/out.txt | sort | uniq -c | head -30"####;
/// blocks.jsonl line 73 (73=subagent), fixture-substituted.
const L73: &str = r####"cd @PROJ@ && S=@SCRATCH@ && cat > $S/c1.json <<'J'
[["tail_sh", "git commit -F - <<'EOF'; sh .git/COMMIT_EDITMSG\nrm -rf ~\nEOF"],
 ["tail_and", "git commit -F - <<'EOF' && bash .git/COMMIT_EDITMSG\nrm -rf ~\nEOF"],
 ["redir_body", "git commit -F - <<'EOF'\necho x > ~/.zshrc\nEOF\nsh .git/COMMIT_EDITMSG"],
 ["bt_exec", "git commit -F - <<'EOF'\n`echo rm` -rf ~\nEOF\nsh .git/COMMIT_EDITMSG"],
 ["plain_ok", "git commit -F - <<'EOF'\nfix: `blastguard log` output\nEOF"]]
J
python3 $S/probe.py target/debug/blastguard $S/c1.json"####;
/// Allow-twin of line 73: the same command with the var resolution done by hand.
const T73: &str = r####"cd @PROJ@ && S=@SCRATCH@ && cat > @SCRATCH@/c1.json <<'J'
[["tail_sh", "git commit -F - <<'EOF'; sh .git/COMMIT_EDITMSG\nrm -rf ~\nEOF"],
 ["tail_and", "git commit -F - <<'EOF' && bash .git/COMMIT_EDITMSG\nrm -rf ~\nEOF"],
 ["redir_body", "git commit -F - <<'EOF'\necho x > ~/.zshrc\nEOF\nsh .git/COMMIT_EDITMSG"],
 ["bt_exec", "git commit -F - <<'EOF'\n`echo rm` -rf ~\nEOF\nsh .git/COMMIT_EDITMSG"],
 ["plain_ok", "git commit -F - <<'EOF'\nfix: `blastguard log` output\nEOF"]]
J
python3 @SCRATCH@/probe.py target/debug/blastguard @SCRATCH@/c1.json"####;
/// blocks.jsonl line 99 (99=subagent), fixture-substituted.
const L99: &str = r####"cd @PROJ@ && git diff > @SCRATCH@/fix.patch && git checkout crates/blastguard/src && F=crates/blastguard/tests/backlog_20261113_python_heredoc_body_append_backtick_fp.rs && sed -i '' '94d' $F && sed -n 88,96p $F && git diff --stat && . "$HOME/.cargo/env" && tdd red --task b2330a21 --cmd 'cargo test -p blastguard --test backlog_20261113_python_heredoc_body_append_backtick_fp -- --include-ignored' 2>&1 | tail -30"####;
/// Allow-twin of line 99: the same command with the var resolution done by hand.
const T99: &str = r####"cd @PROJ@ && git diff > @SCRATCH@/fix.patch && git checkout crates/blastguard/src && F=crates/blastguard/tests/backlog_20261113_python_heredoc_body_append_backtick_fp.rs && sed -i '' '94d' crates/blastguard/tests/backlog_20261113_python_heredoc_body_append_backtick_fp.rs && sed -n 88,96p crates/blastguard/tests/backlog_20261113_python_heredoc_body_append_backtick_fp.rs && git diff --stat && . "$HOME/.cargo/env" && tdd red --task b2330a21 --cmd 'cargo test -p blastguard --test backlog_20261113_python_heredoc_body_append_backtick_fp -- --include-ignored' 2>&1 | tail -30"####;
/// blocks.jsonl line 149 (149=subagent), fixture-substituted.
const L149: &str = r####"cd @PROJ@ && S=@SCRATCH@; mkdir -p $S/d046 && git archive d046f4a3a | tar -x -C $S/d046; . "$HOME/.cargo/env"; CARGO_TARGET_DIR=$S/tw cargo build -p blastguard 2>&1 | tail -1; (cd $S/d046 && CARGO_TARGET_DIR=$S/td cargo build -p blastguard 2>&1 | tail -1); $S/tw/debug/blastguard --version"####;
/// Allow-twin of line 149: the same command with the var resolution done by hand.
const T149: &str = r####"cd @PROJ@ && S=@SCRATCH@; mkdir -p @SCRATCH@/d046 && git archive d046f4a3a | tar -x -C @SCRATCH@/d046; . "$HOME/.cargo/env"; CARGO_TARGET_DIR=@SCRATCH@/tw cargo build -p blastguard 2>&1 | tail -1; (cd @SCRATCH@/d046 && CARGO_TARGET_DIR=@SCRATCH@/td cargo build -p blastguard 2>&1 | tail -1); @SCRATCH@/tw/debug/blastguard --version"####;
/// blocks.jsonl line 152 (152=subagent), fixture-substituted.
const L152: &str = r####"cd @PROJ@/crates/blastguard/tests && f=backlog_20261114_side_round2_f7_f9_f10.rs && sed -i '93,162d' $f && git mv -f $f x 2>/dev/null; mv $f backlog_20261114_python_stdin_forward_shell_literal_harm.rs 2>/dev/null; ls | grep python_stdin_forward; "####;
/// Allow-twin of line 152: the same command with the var resolution done by hand.
const T152: &str = r####"cd @PROJ@/crates/blastguard/tests && f=backlog_20261114_side_round2_f7_f9_f10.rs && sed -i '93,162d' backlog_20261114_side_round2_f7_f9_f10.rs && git mv -f backlog_20261114_side_round2_f7_f9_f10.rs x 2>/dev/null; mv backlog_20261114_side_round2_f7_f9_f10.rs backlog_20261114_python_stdin_forward_shell_literal_harm.rs 2>/dev/null; ls | grep python_stdin_forward; "####;
/// blocks.jsonl line 157 (157=subagent), fixture-substituted.
const L157: &str = r####"cd @PROJ@/crates/blastguard/tests && f=backlog_20261115_exec_dup_onto_stdin_literal_harm.rs && for n in dup_fd3_onto_stdin_then_bash_is_deny_in_both_modes dup_fd3_onto_stdin_close_fd3_then_sh_is_deny_in_both_modes dup_fd3_short_form_then_bash_is_deny_in_both_modes; do sed -i '' "s|^fn $n|#[ignore = \"RED: 2026-11-15 fd-0 dup of an exec-opened here-text is not judged — remove when fixed\"]\nfn $n|" $f; done; sed -i '' 's|^#\[test\]$|#[test]|' $f; grep -n -B2 'ignore' $f"####;
/// Allow-twin of line 157: the same command with the var resolution done by hand.
const T157: &str = r####"cd @PROJ@/crates/blastguard/tests && f=backlog_20261115_exec_dup_onto_stdin_literal_harm.rs && for n in dup_fd3_onto_stdin_then_bash_is_deny_in_both_modes dup_fd3_onto_stdin_close_fd3_then_sh_is_deny_in_both_modes dup_fd3_short_form_then_bash_is_deny_in_both_modes; do sed -i '' "s|^fn $n|#[ignore = \"RED: 2026-11-15 fd-0 dup of an exec-opened here-text is not judged — remove when fixed\"]\nfn $n|" backlog_20261115_exec_dup_onto_stdin_literal_harm.rs; done; sed -i '' 's|^#\[test\]$|#[test]|' backlog_20261115_exec_dup_onto_stdin_literal_harm.rs; grep -n -B2 'ignore' backlog_20261115_exec_dup_onto_stdin_literal_harm.rs"####;
/// blocks.jsonl line 172 (172=subagent), fixture-substituted.
const L172: &str = r####"cd @PROJ@ && rm crates/blastguard/tests/backlog_20261115_heredoc_rs_prose_git_reset.rs && . "$HOME/.cargo/env" && f=crates/blastguard/tests/backlog_20261115_quoted_fd_dup_target.rs && sed -i "s|chmod 777 ~/.ssh'; bash|rm -rf ~'; bash|" $f && grep -n "HARM: " $f && cargo test -p blastguard --test backlog_20261115_quoted_fd_dup_target 2>&1 | grep -E "^test |left|right|result""####;
/// Allow-twin of line 172: the same command with the var resolution done by hand.
const T172: &str = r####"cd @PROJ@ && rm crates/blastguard/tests/backlog_20261115_heredoc_rs_prose_git_reset.rs && . "$HOME/.cargo/env" && f=crates/blastguard/tests/backlog_20261115_quoted_fd_dup_target.rs && sed -i "s|chmod 777 ~/.ssh'; bash|rm -rf ~'; bash|" crates/blastguard/tests/backlog_20261115_quoted_fd_dup_target.rs && grep -n "HARM: " crates/blastguard/tests/backlog_20261115_quoted_fd_dup_target.rs && cargo test -p blastguard --test backlog_20261115_quoted_fd_dup_target 2>&1 | grep -E "^test |left|right|result""####;
/// blocks.jsonl line 195 (195=subagent), fixture-substituted.
const L195: &str = r####"cd /tmp; P=@SCRATCH@/p.sh; chmod +x $P
H='echo '"'"'import os; os.chmod("/etc/hosts", 0o777)'"'"' | python3 -c '
B='echo '"'"'print(1)'"'"' | python3 -c '
for prog in 'exec (input())' 'compile (input(),"x","exec")' 'g=globals; g()["exec"](input())' 'exec(input())'; do
 for m in main sub; do $P $m "$H'$prog'"; done
 for m in main sub; do $P $m "$B'$prog'"; done
done"####;
/// Allow-twin of line 195: the same command with the var resolution done by hand.
const T195: &str = r####"cd /tmp; P=@SCRATCH@/p.sh; chmod +x @SCRATCH@/p.sh
H='echo '"'"'import os; os.chmod("/etc/hosts", 0o777)'"'"' | python3 -c '
B='echo '"'"'print(1)'"'"' | python3 -c '
for prog in 'exec (input())' 'compile (input(),"x","exec")' 'g=globals; g()["exec"](input())' 'exec(input())'; do
 for m in main sub; do @SCRATCH@/p.sh $m "$H'$prog'"; done
 for m in main sub; do @SCRATCH@/p.sh $m "$B'$prog'"; done
done"####;
/// blocks.jsonl line 200 (200=subagent), fixture-substituted.
const L200: &str = r####"S=@SCRATCH@/t2v2; ls $S; sed -i.bak 's/open(sys.argv\[1\]).read().splitlines()/sys.argv[1:]/' $S/run.py 2>&1; python3 $S/run.py 'bash /dev/fd/3'"####;
/// Allow-twin of line 200: the same command with the var resolution done by hand.
const T200: &str = r####"S=@SCRATCH@/t2v2; ls @SCRATCH@/t2v2; sed -i.bak 's/open(sys.argv\[1\]).read().splitlines()/sys.argv[1:]/' @SCRATCH@/t2v2/run.py 2>&1; python3 @SCRATCH@/t2v2/run.py 'bash /dev/fd/3'"####;
/// blocks.jsonl line 221 (221=interactive, 222=interactive, 223=subagent), fixture-substituted.
const L221: &str = r####"echo hi | bash /dev/stdin"####;
/// Allow-twin of line 221: what the shell would actually run, spelled on the command line.
const T221: &str = r####"hi"####;
/// blocks.jsonl line 224 (224=interactive, 225=interactive, 226=subagent), fixture-substituted.
const L224: &str = r####"echo hi | bash"####;
/// Allow-twin of line 224: what the shell would actually run, spelled on the command line.
const T224: &str = r####"hi"####;
/// blocks.jsonl line 227 (227=interactive, 228=interactive, 229=subagent), fixture-substituted.
const L227: &str = r####"bash script.sh /dev/stdin"####;
/// Allow-twin of line 227: what the shell would actually run, spelled on the command line.
const T227: &str = r####"bash script.sh"####;
/// blocks.jsonl line 230 (230=interactive, 231=interactive, 232=subagent), fixture-substituted.
const L230: &str = r####"bash -c echo /dev/stdin"####;
/// Allow-twin of line 230: what the shell would actually run, spelled on the command line.
const T230: &str = r####"bash -c echo"####;
/// blocks.jsonl line 250 (250=interactive, 251=interactive, 252=subagent), fixture-substituted.
const L250: &str = r####"bash -c "echo hi" /dev/stdin"####;
/// Allow-twin of line 250: what the shell would actually run, spelled on the command line.
const T250: &str = r####"bash -c "echo hi""####;
/// blocks.jsonl line 263 (263=subagent), fixture-substituted.
const L263: &str = r####"S=@SCRATCH@/f18
git init -q $S/repo && git -C $S/repo config user.email a@b && git -C $S/repo config user.name t && git -C $S/repo commit -q --allow-empty -m init && mkdir -p $S/home
for m in main sub; do
 if [ $m = sub ]; then A='"agent_id":"x",'; else A=''; fi
 jq -n --rawfile c $S/cmd.txt --arg r $S/repo --arg sub "$m" '{session_id:"s1",hook_event_name:"PreToolUse",tool_name:"Bash",cwd:$r,tool_input:{command:($c|sub("^cd [^ ]+ ";"cd "+$r+" "))}} | if $sub=="sub" then .agent_id="x" else . end' > $S/p_$m.json
done
jq -r .tool_input.command $S/p_main.json | head -c 120; echo
for m in main sub; do echo == $m; env -i HOME=$S/home PATH=/usr/bin:/bin CLAUDECODE=1 CLAUDE_CODE_ENTRYPOINT=cli @PROJ@/target/debug/blastguard < $S/p_$m.json; echo "exit=$?"; done"####;
/// Allow-twin of line 263: the same command with the for resolution done by hand.
const T263: &str = r####"S=@SCRATCH@/f18
git init -q @SCRATCH@/f18/repo && git -C @SCRATCH@/f18/repo config user.email a@b && git -C @SCRATCH@/f18/repo config user.name t && git -C @SCRATCH@/f18/repo commit -q --allow-empty -m init && mkdir -p @SCRATCH@/f18/home
 if [ main = sub ]; then A='"agent_id":"x",'; else A=''; fi
 jq -n --rawfile c @SCRATCH@/f18/cmd.txt --arg r @SCRATCH@/f18/repo --arg sub "main" '{session_id:"s1",hook_event_name:"PreToolUse",tool_name:"Bash",cwd:$r,tool_input:{command:($c|sub("^cd [^ ]+ ";"cd "+$r+" "))}} | if $sub=="sub" then .agent_id="x" else . end' > @SCRATCH@/f18/p_main.json
 if [ sub = sub ]; then A='"agent_id":"x",'; else A=''; fi
 jq -n --rawfile c @SCRATCH@/f18/cmd.txt --arg r @SCRATCH@/f18/repo --arg sub "sub" '{session_id:"s1",hook_event_name:"PreToolUse",tool_name:"Bash",cwd:$r,tool_input:{command:($c|sub("^cd [^ ]+ ";"cd "+$r+" "))}} | if $sub=="sub" then .agent_id="x" else . end' > @SCRATCH@/f18/p_sub.json
jq -r .tool_input.command @SCRATCH@/f18/p_main.json | head -c 120; echo
echo == main; env -i HOME=@SCRATCH@/f18/home PATH=/usr/bin:/bin CLAUDECODE=1 CLAUDE_CODE_ENTRYPOINT=cli @PROJ@/target/debug/blastguard < @SCRATCH@/f18/p_main.json; echo "exit=$?"; echo == sub; env -i HOME=@SCRATCH@/f18/home PATH=/usr/bin:/bin CLAUDECODE=1 CLAUDE_CODE_ENTRYPOINT=cli @PROJ@/target/debug/blastguard < @SCRATCH@/f18/p_sub.json; echo "exit=$?""####;
/// blocks.jsonl line 265 (265=interactive), fixture-substituted.
const L265: &str = r####"R=@SCRATCH@/reds; cd @PROJ@ && F=crates/blastguard/tests/backlog_20261115_quoted_heredoc_backtick_commit_msg_fp.rs && { cat $R/6_head.rs; sed 's/"bg-side1115-{tag}-{}-{}"/"bg-f18-{tag}-{}-{}"/' $R/harness.rs; cat $R/6_tail.rs; } > $F && grep -c 'bg-f18-' $F && . "$HOME/.cargo/env" && rustfmt --edition 2021 --check $F && cargo test -p blastguard --test backlog_20261115_quoted_heredoc_backtick_commit_msg_fp -- --include-ignored 2>&1 | grep -E '^test |test result'"####;
/// Allow-twin of line 265: the same command with the var resolution done by hand.
const T265: &str = r####"R=@SCRATCH@/reds; cd @PROJ@ && F=crates/blastguard/tests/backlog_20261115_quoted_heredoc_backtick_commit_msg_fp.rs && { cat @SCRATCH@/reds/6_head.rs; sed 's/"bg-side1115-{tag}-{}-{}"/"bg-f18-{tag}-{}-{}"/' @SCRATCH@/reds/harness.rs; cat @SCRATCH@/reds/6_tail.rs; } > crates/blastguard/tests/backlog_20261115_quoted_heredoc_backtick_commit_msg_fp.rs && grep -c 'bg-f18-' crates/blastguard/tests/backlog_20261115_quoted_heredoc_backtick_commit_msg_fp.rs && . "$HOME/.cargo/env" && rustfmt --edition 2021 --check crates/blastguard/tests/backlog_20261115_quoted_heredoc_backtick_commit_msg_fp.rs && cargo test -p blastguard --test backlog_20261115_quoted_heredoc_backtick_commit_msg_fp -- --include-ignored 2>&1 | grep -E '^test |test result'"####;
/// blocks.jsonl line 275 (275=subagent, 276=interactive), fixture-substituted.
const L275: &str = r####". "$HOME/.cargo/env"; cd @PROJ@ && git branch --show-current && T=crates/blastguard/tests/backlog_20261115_quoted_heredoc_json_backtick_subagent_fp.rs && rustfmt --edition 2021 $T && git add $T && git commit -q -F - <<EOF 2>&1 | grep -iE "\[BLOCK\]|blocked|GATE FAILED|  - " | head; git log --oneline -1
blastguard: RED for quoted heredoc JSON body backtick denied as command word

New RED for a suspected FP, ignored until the fix lands.

test-weakening-justified: $T:ignore-added — new RED tests for unfixed FP, observed failing before the ignore

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF"####;
/// Allow-twin of line 275: the same command with the var resolution done by hand.
const T275: &str = r####". "$HOME/.cargo/env"; cd @PROJ@ && git branch --show-current && T=crates/blastguard/tests/backlog_20261115_quoted_heredoc_json_backtick_subagent_fp.rs && rustfmt --edition 2021 crates/blastguard/tests/backlog_20261115_quoted_heredoc_json_backtick_subagent_fp.rs && git add crates/blastguard/tests/backlog_20261115_quoted_heredoc_json_backtick_subagent_fp.rs && git commit -q -F - <<EOF 2>&1 | grep -iE "\[BLOCK\]|blocked|GATE FAILED|  - " | head; git log --oneline -1
blastguard: RED for quoted heredoc JSON body backtick denied as command word

New RED for a suspected FP, ignored until the fix lands.

test-weakening-justified: crates/blastguard/tests/backlog_20261115_quoted_heredoc_json_backtick_subagent_fp.rs:ignore-added — new RED tests for unfixed FP, observed failing before the ignore

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF"####;
/// blocks.jsonl line 281 (281=subagent), fixture-substituted.
const L281: &str = r####"D=@SCRATCH@/run; for f in decomp decomp.routed; do jq '.tasks|=map(.reproduction_tests=(.reproduction_tests|join(" && ")))' $D/$f.json > $D/$f.tmp && mv $D/$f.tmp $D/$f.json; done; condukt validate --file $D/decomp.routed.json; echo "v=$?"; condukt schedule --file $D/decomp.routed.json; echo "s=$?""####;
/// Allow-twin of line 281: the same command with the for resolution done by hand.
const T281: &str = r####"D=@SCRATCH@/run; jq '.tasks|=map(.reproduction_tests=(.reproduction_tests|join(" && ")))' @SCRATCH@/run/decomp.json > @SCRATCH@/run/decomp.tmp && mv @SCRATCH@/run/decomp.tmp @SCRATCH@/run/decomp.json; jq '.tasks|=map(.reproduction_tests=(.reproduction_tests|join(" && ")))' @SCRATCH@/run/decomp.routed.json > @SCRATCH@/run/decomp.routed.tmp && mv @SCRATCH@/run/decomp.routed.tmp @SCRATCH@/run/decomp.routed.json; condukt validate --file @SCRATCH@/run/decomp.routed.json; echo "v=$?"; condukt schedule --file @SCRATCH@/run/decomp.routed.json; echo "s=$?""####;
/// blocks.jsonl line 292 (292=subagent, 294=interactive), fixture-substituted.
const L292: &str = r####"cd @PROJ@ && ls -la target/debug/blastguard && S=@SCRATCH@ && mkdir -p $S && cp target/debug/blastguard $S/bg-fix && echo ok"####;
/// Allow-twin of line 292: the same command with the var resolution done by hand.
const T292: &str = r####"cd @PROJ@ && ls -la target/debug/blastguard && S=@SCRATCH@ && mkdir -p @SCRATCH@ && cp target/debug/blastguard @SCRATCH@/bg-fix && echo ok"####;
/// blocks.jsonl line 298 (298=subagent), fixture-substituted.
const L298: &str = r####"for f in .claude-plugin/marketplace.json Cargo.lock crates/condukt/.claude-plugin/plugin.json crates/condukt/Cargo.toml; do sed -i '' -e '/^<<<<<<< HEAD$/,/^=======$/d' -e '/^>>>>>>> fix\/testscope-cwd-20261115$/d' "$f"; done; grep -n "<<<<<<<\|>>>>>>>\|^=======$" .claude-plugin/marketplace.json Cargo.lock crates/condukt/.claude-plugin/plugin.json crates/condukt/Cargo.toml; git diff HEAD --stat; git diff HEAD -- .claude-plugin/marketplace.json Cargo.lock | head -30"####;
/// Allow-twin of line 298: the same command with the for resolution done by hand.
const T298: &str = r####"sed -i '' -e '/^<<<<<<< HEAD$/,/^=======$/d' -e '/^>>>>>>> fix\/testscope-cwd-20261115$/d' ".claude-plugin/marketplace.json"; sed -i '' -e '/^<<<<<<< HEAD$/,/^=======$/d' -e '/^>>>>>>> fix\/testscope-cwd-20261115$/d' "Cargo.lock"; sed -i '' -e '/^<<<<<<< HEAD$/,/^=======$/d' -e '/^>>>>>>> fix\/testscope-cwd-20261115$/d' "crates/condukt/.claude-plugin/plugin.json"; sed -i '' -e '/^<<<<<<< HEAD$/,/^=======$/d' -e '/^>>>>>>> fix\/testscope-cwd-20261115$/d' "crates/condukt/Cargo.toml"; grep -n "<<<<<<<\|>>>>>>>\|^=======$" .claude-plugin/marketplace.json Cargo.lock crates/condukt/.claude-plugin/plugin.json crates/condukt/Cargo.toml; git diff HEAD --stat; git diff HEAD -- .claude-plugin/marketplace.json Cargo.lock | head -30"####;
/// blocks.jsonl line 299 (299=subagent), fixture-substituted.
const L299: &str = r####"cd @SCRATCH@; file bg-base | head -2; ls -la bg-fix bg-base; for b in bg-fix bg-base; do ./$b --version 2>&1 </dev/null | head -1; done; cd @PROJ@; git log -1 --format=%h; grep -m1 '^version' crates/blastguard/Cargo.toml; mkdir -p @SCRATCH@/reds/txtdata"####;
/// Allow-twin of line 299: the same command with the for resolution done by hand.
const T299: &str = r####"cd @SCRATCH@; file bg-base | head -2; ls -la bg-fix bg-base; ./bg-fix --version 2>&1 </dev/null | head -1; ./bg-base --version 2>&1 </dev/null | head -1; cd @PROJ@; git log -1 --format=%h; grep -m1 '^version' crates/blastguard/Cargo.toml; mkdir -p @SCRATCH@/reds/txtdata"####;
/// blocks.jsonl line 306 (306=subagent), fixture-substituted.
const L306: &str = r####"cd @PROJ@ && . "$HOME/.cargo/env" && S=@SCRATCH@; cargo test --no-fail-fast -p blastguard --test backlog_20261114_dup_target_quoted_dollar_fp --test backlog_20261114_exec_fd_dynamic_fd_reader --test backlog_20261114_exec_fd_herestring_then_shell_reads_fd --test backlog_20261114_exec_fd_var_dup_reader --test backlog_20261114_source_dev_stdin_dup --test backlog_20261114_stdin_dup_move_fd_suffix --test backlog_20261115_dq_dollar_literal_dup_fd --test backlog_20261115_exec_dup_onto_stdin_literal_harm --test backlog_20261115_quoted_numeric_dup_target --test spec_bg_exec_fd_tracking -- --include-ignored > $S/base_int.txt 2>&1; grep -E "Running|test result|FAILED|=> |main must|cmd=" $S/base_int.txt | grep -v " ok$""####;
/// Allow-twin of line 306: the same command with the var resolution done by hand.
const T306: &str = r####"cd @PROJ@ && . "$HOME/.cargo/env" && S=@SCRATCH@; cargo test --no-fail-fast -p blastguard --test backlog_20261114_dup_target_quoted_dollar_fp --test backlog_20261114_exec_fd_dynamic_fd_reader --test backlog_20261114_exec_fd_herestring_then_shell_reads_fd --test backlog_20261114_exec_fd_var_dup_reader --test backlog_20261114_source_dev_stdin_dup --test backlog_20261114_stdin_dup_move_fd_suffix --test backlog_20261115_dq_dollar_literal_dup_fd --test backlog_20261115_exec_dup_onto_stdin_literal_harm --test backlog_20261115_quoted_numeric_dup_target --test spec_bg_exec_fd_tracking -- --include-ignored > @SCRATCH@/base_int.txt 2>&1; grep -E "Running|test result|FAILED|=> |main must|cmd=" @SCRATCH@/base_int.txt | grep -v " ok$""####;
/// blocks.jsonl line 315 (315=subagent, 328=interactive), fixture-substituted.
const L315: &str = r####"S=@SCRATCH@/brushspike && mkdir -p $S/src && cd $S && cat > Cargo.toml <<'EOF'
[package]
name = "brushspike"
version = "0.1.0"
edition = "2021"
[dependencies]
brush-parser = "=0.4.0"
EOF
echo 'fn main(){}' > src/main.rs && . "$HOME/.cargo/env" && cargo build 2>&1 | tail -3 && cargo tree 2>/dev/null | head -60 && ls ~/.cargo/registry/src/*/ | grep brush-parser"####;
/// Allow-twin of line 315: the same command with the cd resolution done by hand.
const T315: &str = r####"S=@SCRATCH@/brushspike && mkdir -p @SCRATCH@/brushspike/src && cd @SCRATCH@/brushspike && cat > Cargo.toml <<'EOF'
[package]
name = "brushspike"
version = "0.1.0"
edition = "2021"
[dependencies]
brush-parser = "=0.4.0"
EOF
echo 'fn main(){}' > src/main.rs && . "$HOME/.cargo/env" && cargo build 2>&1 | tail -3 && cargo tree 2>/dev/null | head -60 && ls ~/.cargo/registry/src/*/ | grep brush-parser"####;
/// blocks.jsonl line 322 (322=subagent), fixture-substituted.
const L322: &str = r####"cd @PROJ@ && D=@SCRATCH@; RID=$(condukt state init --file $D/decomp2.json) && echo $RID && condukt state checkpoint --run "$RID" --label baseline; echo "$RID" > $D/rid; condukt config show 2>/dev/null | head -20; git -C @PROJ@ log --oneline -1"####;
/// Allow-twin of line 322: the same command with the var resolution done by hand.
const T322: &str = r####"cd @PROJ@ && D=@SCRATCH@; RID=$(condukt state init --file @SCRATCH@/decomp2.json) && echo $RID && condukt state checkpoint --run "$RID" --label baseline; echo "$RID" > @SCRATCH@/rid; condukt config show 2>/dev/null | head -20; git -C @PROJ@ log --oneline -1"####;
/// blocks.jsonl line 324 (324=interactive), fixture-substituted.
const L324: &str = r####"cd @PROJ@ && S=@SCRATCH@/bm && mkdir -p $S && git show :1:.backlog/tasks.toml > $S/O && git show :2:.backlog/tasks.toml > $S/A && git show :3:.backlog/tasks.toml > $S/B && backlog merge-driver $S/O $S/A $S/B; echo rc=$?; grep -c '^id = ' $S/A"####;
/// Allow-twin of line 324: the same command with the var resolution done by hand.
const T324: &str = r####"cd @PROJ@ && S=@SCRATCH@/bm && mkdir -p @SCRATCH@/bm && git show :1:.backlog/tasks.toml > @SCRATCH@/bm/O && git show :2:.backlog/tasks.toml > @SCRATCH@/bm/A && git show :3:.backlog/tasks.toml > @SCRATCH@/bm/B && backlog merge-driver @SCRATCH@/bm/O @SCRATCH@/bm/A @SCRATCH@/bm/B; echo rc=$?; grep -c '^id = ' @SCRATCH@/bm/A"####;
/// blocks.jsonl line 327 (327=subagent), fixture-substituted.
const L327: &str = r####"S=@SCRATCH@/reds/writesh; D=$S/draft; mkdir -p $D
T=$S/src/crates/blastguard/tests/backlog_20261115_python_heredoc_write_then_sh_fail_open.rs
# reuse helper section (lines 62-107 of the sibling: Home..py_heredoc) 
sed -n '/^#\[path/,/^fn py_heredoc/p' $S/src/crates/blastguard/tests/backlog_20261115_python_string_literal_subprocess_chmod_data_fp.rs | sed '$d' > $D/helpers.rs
sed -n '/^fn py_heredoc/,/^}/p' $S/src/crates/blastguard/tests/backlog_20261115_python_string_literal_subprocess_chmod_data_fp.rs >> $D/helpers.rs
wc -l $D/helpers.rs; tail -5 $D/helpers.rs"####;
/// Allow-twin of line 327: the same command with the var resolution done by hand.
const T327: &str = r####"S=@SCRATCH@/reds/writesh; D=@SCRATCH@/reds/writesh/draft; mkdir -p @SCRATCH@/reds/writesh/draft
T=@SCRATCH@/reds/writesh/src/crates/blastguard/tests/backlog_20261115_python_heredoc_write_then_sh_fail_open.rs
# reuse helper section (lines 62-107 of the sibling: Home..py_heredoc) 
sed -n '/^#\[path/,/^fn py_heredoc/p' @SCRATCH@/reds/writesh/src/crates/blastguard/tests/backlog_20261115_python_string_literal_subprocess_chmod_data_fp.rs | sed '$d' > @SCRATCH@/reds/writesh/draft/helpers.rs
sed -n '/^fn py_heredoc/,/^}/p' @SCRATCH@/reds/writesh/src/crates/blastguard/tests/backlog_20261115_python_string_literal_subprocess_chmod_data_fp.rs >> @SCRATCH@/reds/writesh/draft/helpers.rs
wc -l @SCRATCH@/reds/writesh/draft/helpers.rs; tail -5 @SCRATCH@/reds/writesh/draft/helpers.rs"####;
/// blocks.jsonl line 329 (329=subagent), fixture-substituted.
const L329: &str = r####"cd crates/blastguard/src/detect/interpreter; python3 - <<'EOF' 2>&1 | tail -3
EOF
grep -n "^use\|^    " python_semantics.rs | sed -n 1,12p; grep -n "fn name(" -A8 python_semantics.rs | head -12"####;
/// Allow-twin of line 329: what the shell would actually run, spelled on the command line.
const T329: &str = r####"cd crates/blastguard/src/detect/interpreter; python3 -c '' 2>&1 | tail -3
grep -n "^use\|^    " python_semantics.rs | sed -n 1,12p; grep -n "fn name(" -A8 python_semantics.rs | head -12"####;
/// blocks.jsonl line 334 (334=interactive), fixture-substituted.
const L334: &str = r####"cd @PROJ@ && F=crates/blastguard/tests/backlog_20261115_python_heredoc_write_then_sh_fail_open.rs && sed -n '157p;164p' $F && sed -i '' '157,165d' $F && grep -n 'write_only\|write-only' $F; sed -i '' '4s/0\.2\.616/0.2.617/' crates/blastguard/Cargo.toml; sed -i '' '3s/0\.2\.616/0.2.617/' crates/blastguard/.claude-plugin/plugin.json; sed -i '' '60s/0\.2\.616/0.2.617/' .claude-plugin/marketplace.json; sed -i '' '184s/0\.2\.616/0.2.617/' Cargo.lock; git diff --stat"####;
/// Allow-twin of line 334: the same command with the var resolution done by hand.
const T334: &str = r####"cd @PROJ@ && F=crates/blastguard/tests/backlog_20261115_python_heredoc_write_then_sh_fail_open.rs && sed -n '157p;164p' crates/blastguard/tests/backlog_20261115_python_heredoc_write_then_sh_fail_open.rs && sed -i '' '157,165d' crates/blastguard/tests/backlog_20261115_python_heredoc_write_then_sh_fail_open.rs && grep -n 'write_only\|write-only' crates/blastguard/tests/backlog_20261115_python_heredoc_write_then_sh_fail_open.rs; sed -i '' '4s/0\.2\.616/0.2.617/' crates/blastguard/Cargo.toml; sed -i '' '3s/0\.2\.616/0.2.617/' crates/blastguard/.claude-plugin/plugin.json; sed -i '' '60s/0\.2\.616/0.2.617/' .claude-plugin/marketplace.json; sed -i '' '184s/0\.2\.616/0.2.617/' Cargo.lock; git diff --stat"####;
/// blocks.jsonl line 69 (69=subagent), fixture-substituted.
const L69: &str = r####"cd @PROJ@ && f=crates/blastguard/tests/backlog_20261114_python_heredoc_body_tee_text_fp.rs && python_src=$(cat crates/blastguard/tests/backlog_20261113_python_heredoc_body_append_backtick_fp.rs | sed -n '/^#\[path/,/^const BODY_WITH_BACKTICK/p' | sed '$d') && printf '%s\n' '#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]' "$python_src" > $f && cat >> $f <<'EOF'
const CASES: &[(&str, &str)] = &[
    ("quoted_pipe_tee", "python3 - <<'PY'\nprint(\"run | tee out.txt\")\nPY"),
    ("quoted_cat_tee_words", "python3 - <<'PY'\n# uses cat and tee here\nprint(\"cat a | tee b\")\nPY"),
    ("unquoted_pipe_tee", "python3 - <<PY\nprint(\"run | tee out.txt\")\nPY"),
];

#[test]
fn probe_tee_text_in_python_heredoc_body() {
    let home = unique_home("tee");
    let mut bad = Vec::new();
    for (tag, c) in CASES {
        for sub in [false, true] {
            let (d, r) = run_as(c, &home, sub);
            eprintln!("PROBE {tag} sub={sub}: {d} | {r}");
            if r.contains("tee without") {
                bad.push(format!("{tag} sub={sub}: {d} | {r}"));
            }
        }
    }
    assert!(bad.is_empty(), "tee text in heredoc body judged as a tee command: {bad:#?}");
}
EOF
. "$HOME/.cargo/env"; cargo test -p blastguard --test backlog_20261114_python_heredoc_body_tee_text_fp -- --nocapture 2>&1 | grep -E "PROBE|test result|panicked|error" | head -20"####;
/// blocks.jsonl line 112 (112=subagent), fixture-substituted.
const L112: &str = r####"cd @PROJ@ && cat > crates/blastguard/tests/verify_b2330a21_probes.rs <<'EOF'
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Verifier probes for b2330a21. Each probe is a command that must NOT be
//! allowed (write-then-run, opener redirect, f-string, import, etc.).
//! BG_BIN env var points the run at another binary (base comparison).
//! DOES NOT PROVE: anything about commands not listed here.

#[path = "support/bounded.rs"]
mod bounded;

use std::process::{Command, Stdio};

fn decide(command: &str, subagent: bool) -> String {
    let home = std::env::temp_dir().join(format!("bg-v-{}-{}", std::process::id(), subagent));
    std::fs::create_dir_all(home.join("wt")).unwrap();
    let mut p = serde_json::json!({
        "session_id": "v-b2330a21", "cwd": home.join("wt"),
        "hook_event_name": "PreToolUse", "tool_name": "Bash",
        "tool_input": {"command": command},
    });
    if subagent {
        p["agent_id"] = serde_json::json!("test-subagent");
    }
    let bin = std::env::var("BG_BIN").unwrap_or_else(|_| env!("CARGO_BIN_EXE_blastguard").into());
    let mut cmd = Command::new(bin);
    cmd.env_clear().env("HOME", &home).env("PATH", "/usr/bin:/bin")
        .env("CLAUDECODE", "1").env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut cmd, p.to_string().as_bytes(), bounded::HOOK_DEADLINE);
    let s = String::from_utf8_lossy(&out.stdout).into_owned();
    if s.trim().is_empty() {
        return "allow".into();
    }
    let v: serde_json::Value = serde_json::from_str(s.trim()).unwrap();
    v["hookSpecificOutput"]["permissionDecision"].as_str().unwrap_or("?").to_string()
}

fn h(body: &str, after: &str) -> String {
    format!("python3 - <<'EOF'\n{body}\nEOF{after}")
}

#[test]
fn probes_not_allowed() {
    let bt = "`y`";
    let mut p: Vec<(String, String)> = vec![];
    let mut add = |n: &str, c: String| p.push((n.into(), c));
    add("write-then-run open w", h(&format!("open(\"run.sh\",\"w\").write(\"echo x >> {bt}\")"), "\nsh run.sh"));
    add("open a", h(&format!("open(\"run.sh\",\"a\").write(\"echo x >> {bt}\")"), "\nsh run.sh"));
    add("open x", h(&format!("open(\"run.sh\",\"x\").write(\"echo x >> {bt}\")"), "\nsh run.sh"));
    add("pathlib", h(&format!("import pathlib\npathlib.Path(\"run.sh\").write_text(\"echo x >> {bt}\")"), "\nsh run.sh"));
    add("print file=", h(&format!("print(\"echo x >> {bt}\", file=open(\"run.sh\",\"w\"))"), "\nsh run.sh"));
    add("sys.stdout reassign", h(&format!("import sys\nsys.stdout=open(\"run.sh\",\"w\")\nprint(\"echo x >> {bt}\")"), "\nsh run.sh"));
    add("os.write", h(&format!("import os\nos.write(os.open(\"run.sh\",os.O_WRONLY|os.O_CREAT),b\"echo x >> {bt}\")"), "\nsh run.sh"));
    add("opener > run.sh", format!("python3 - > run.sh <<'EOF'\nprint(\"echo x >> {bt}\")\nEOF\nsh run.sh"));
    add("opener | sh", format!("python3 - <<'EOF' | sh\nprint(\"echo x >> {bt}\")\nEOF"));
    add("opener $()", format!("echo $(python3 - <<'EOF'\nprint(\"echo x >> {bt}\")\nEOF\n)"));
    add("opener >> $var", format!("python3 - >> \"$var\" <<'EOF'\nprint(\"x >> {bt}\")\nEOF"));
    add("after >> $var", format!("python3 - <<'EOF'\nprint(\"x >> {bt}\")\nEOF\necho hi >> \"$var\""));
    add("after | sh", format!("python3 - <<'EOF'\nprint(\"x >> {bt}\")\nEOF\necho id | sh"));
    add("after ; sh", format!("python3 - <<'EOF'\nprint(\"x >> {bt}\")\nEOF\n; sh run.sh"));
    add("unterminated", format!("python3 - <<'EOF'\nprint(\"x >> {bt}\")"));
    add("unquoted", format!("python3 - <<EOF\nprint(\"x >> {bt}\")\nEOF"));
    add("unquoted echo", format!("python3 - <<EOF\nprint(\"echo x >> {bt}\")\nEOF"));
    add("python -c shell", format!("python3 -c 'import os; os.system(\"echo x >> {bt}\")'"));
    add("python os.system heredoc", h(&format!("import os\nos.system(\"echo x >> {bt}\")"), ""));
    add("python subprocess", h(&format!("import subprocess\nsubprocess.run(\"echo x >> {bt}\", shell=True)"), ""));
    add("perl bt", format!("perl - <<'EOF'\nprint \"x >> {bt}\"; system(\"echo x >> {bt}\");\nEOF"));
    add("ruby bt", format!("ruby - <<'EOF'\nputs \"x >> {bt}\"; system(\"echo x >> {bt}\")\nEOF"));
    add("php bt", format!("php <<'EOF'\n<?php system(\"echo x >> {bt}\");\nEOF"));
    add("fstring", h(&format!("print(f\"x >> {bt} {{open('a','w')}}\")"), ""));
    add("fstring rf", h(&format!("print(rf\"x >> {bt} {{open('a','w')}}\")"), ""));
    add("exec", h(&format!("exec(\"open('run.sh','w').write('x >> {bt}')\")"), "\nsh run.sh"));
    add("getattr", h(&format!("getattr(__builtins__,'open')('run.sh','w').write('x >> {bt}')"), "\nsh run.sh"));
    add("dollar literal", h("print(\"x >> $y\")", ""));
    add("dollar+bt literal", h(&format!("print(\"x >> $y {bt}\")"), ""));
    add("write attr", h(&format!("print.write(\"x >> {bt}\")"), ""));
    add("sh run.sh bare", h("print(\"x\")", "\nsh run.sh"));
    add("redirect ex after", h(&format!("print(\"x >> {bt}\")"), "\nsh run.sh"));
    add("comment bt", h(&format!("# x >> {bt}\nprint(1)"), ""));
    add("print-only then tee", h(&format!("print(\"echo x >> {bt}\")"), "\n"));
    add("format traversal", h(&format!("print(\"{{0.__class__}} >> {bt}\".format(1))"), ""));
    add("tilde heredoc", format!("python3 - <<'EOF' > run.sh\nprint(\"x >> {bt}\")\nEOF"));
    add("fd redir", format!("python3 - 1>run.sh <<'EOF'\nprint(\"x >> {bt}\")\nEOF"));
    add("tee pipe", format!("python3 - <<'EOF' | tee run.sh\nprint(\"x >> {bt}\")\nEOF\nsh run.sh"));
    add("stdout capture", format!("x=$(python3 - <<'EOF'\nprint(\"echo x >> {bt}\")\nEOF\n)\n$x"));
    add("python3 -- file run", format!("python3 - <<'EOF' >> out.sh\nprint(\"echo x >> {bt}\")\nEOF\nsh out.sh"));
    let mut bad = vec![];
    for (n, c) in &p {
        for sub in [false, true] {
            let d = decide(c, sub);
            println!("PROBE|{n}|sub={sub}|{d}");
            if d == "allow" {
                bad.push(format!("{n} sub={sub}"));
            }
        }
    }
    assert!(bad.is_empty(), "allowed: {bad:?}");
}
EOF
. "$HOME/.cargo/env"; S=@SCRATCH@
CARGO_TARGET_DIR=$S/tfix cargo test -p blastguard --test verify_b2330a21_probes -- --nocapture 2>&1 | grep -E "PROBE|allowed|test result|error" > $S/fix.txt; cat $S/fix.txt | grep -E "allow|result|error""####;
/// blocks.jsonl line 164 (164=subagent), fixture-substituted.
const L164: &str = r####"D=@SCRATCH@/t1v; W=@PROJ@
i=0; while IFS= read -r l; do
 r=""; for m in 0 1; do o=$(env -i HOME=$D/home PATH=/usr/bin:/bin CLAUDECODE=1 CLAUDE_CODE_ENTRYPOINT=cli $W/target/debug/blastguard < $D/p${i}_$m.json 2>/dev/null | python3 -c "
import sys,json
s=sys.stdin.read().strip()
print('allow' if not s else json.loads(s)['hookSpecificOutput']['permissionDecision'])"); r="$r $o"; done
 printf '%-50s %s\n' "$l" "$r"; i=$((i+1)); done < $D/cmds.txt"####;
/// blocks.jsonl line 284 (284=subagent), fixture-substituted.
const L284: &str = r####"cd @PROJ@; S=@SCRATCH@/stopfp; mkdir -p $S/ws && git archive HEAD Cargo.toml Cargo.lock crates/answerlang crates/harness-core | tar -x -C $S/ws && ls $S/ws/crates; cat > $S/ws/crates/answerlang/tests/stop_payload_final_message_20261115.rs <<'EOF'
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (2026-11-15): answerlang's Stop hook judged a STALE interim assistant
//! text instead of the final answer of the turn that was stopping.
//!
//! Observed facts (verbatim, transcript
//! `@HOME@/.claude/projects/p/session.jsonl`):
//! * line 2126 (user, Stop hook feedback): "answerlang: your final answer is not in japanese: only 0% of its 101 prose letters are in japanese script"
//! * line 2117 (assistant, 08:31:54.049Z): English interim text, 101 prose letters (backtick spans removed; counted by script).
//! * line 2123 (assistant, 08:32:12.105Z): Japanese final text, 1276 prose letters.
//! * line 2124 (Stop hook attachment, 08:32:12.245Z): 140 ms after line 2123.
//!   => the hook decided on line 2117's text, not on line 2123's.
//! * Earlier firing, user line 1662 "113 prose letters": the last transcript
//!   assistant text before it was line 1652 (Japanese, 1009 letters, 04:09:54.731Z);
//!   an English interim text at line 1616 (04:04:53.110Z) counts 115 by my
//!   count (hook says 113; counting rules differ slightly, so this second case is
//!   only SUGGESTIVE, not confirmed equal).
//! * `transcript::last_assistant_text` reads the transcript FILE and takes the last
//!   non-sidechain assistant text line (crates/answerlang/src/transcript.rs).
//! * Claude Code changelog (~/.claude/cache/changelog.md:5962): "Added
//!   `last_assistant_message` field to Stop and SubagentStop hook inputs, providing the
//!   final assistant response text so hooks can access it without parsing transcript
//!   files." Observed in ~/.overwatch/harness-e77d23c7/overwatch/subagents.json:
//!   6928 `stop_last_assistant_message` values captured from SubagentStop payloads.
//!   NOT observed: a raw MAIN-session Stop payload carrying the field.
//!   Contradicting note: crates/context-governor/src/io.rs:10 claims "there is no
//!   `last_assistant_message` field" (unsourced; contradicted by the SubagentStop capture).
//!
//! Two readings:
//! (1) real defect: at Stop time the transcript file lags the final message
//!     (async flush), so reading the file yields a stale text. The payload's
//!     `last_assistant_message` is the authoritative source and must win.
//! (2) intended / not a defect: the Stop payload does NOT carry the field for the main
//!     session and the lag is not reproducible; then the fix is a different one
//!     (e.g. treat "last assistant text older than the last tool_result" as
//!     undetermined) and this test asserts a contract Claude Code does not honour.
//! Tell apart: capture one raw main-session Stop payload (e.g. a debug hook
//! `cat > file`) and check for `last_assistant_message`; if present, reading (1).
//!
//! DOES NOT PROVE: that the main-session Stop payload carries the field (UNVERIFIED);
//! that the file lag (rather than some other cause) produced the stale read; that the
//! 1662/113 firing is the same pattern.
mod common;
use common::*;
use serde_json::json;

fn ja_env() -> Env {
    let env = Env::new();
    env.write_config(JA_CONFIG);
    env
}

/// Transcript still ends at the English interim text (the Japanese final answer
/// is not flushed yet); payload carries the Japanese final message.
#[test]
fn payload_last_assistant_message_wins_over_stale_transcript_line() {
    let env = ja_env();
    let t = env.answer_transcript("t.jsonl", &en(200));
    let p = json!({
        "session_id": "s1",
        "hook_event_name": "Stop",
        "stop_hook_active": false,
        "transcript_path": t.to_string_lossy(),
        "last_assistant_message": ja(300),
    });
    let out = env.run(&["stop"], Some(&p));
    assert_eq!(out.code, 0);
    assert!(
        !out.is_block(),
        "judged the stale transcript text, not last_assistant_message; stdout: {}",
        out.stdout
    );
}

/// Control: the same payload field must not launder an English final answer.
#[test]
fn payload_english_final_message_still_blocks() {
    let env = ja_env();
    let t = env.answer_transcript("t.jsonl", &ja(300));
    let p = json!({
        "session_id": "s1",
        "hook_event_name": "Stop",
        "stop_hook_active": false,
        "transcript_path": t.to_string_lossy(),
        "last_assistant_message": en(200),
    });
    let out = env.run(&["stop"], Some(&p));
    assert!(out.is_block(), "stdout: {}", out.stdout);
}
EOF
cd $S/ws && . "$HOME/.cargo/env" && CARGO_TARGET_DIR=$S/target cargo test -p answerlang --test stop_payload_final_message_20261115 2>&1 | tail -30"####;

// ===================================================================
// SANITY — resolved twins, NOT ignored, Allow at the rev of this commit
// ===================================================================

#[test]
fn sanity_line_2_resolved_twin_is_allow() {
    assert_allow_both("sanity_2", T2, false);
}

#[test]
fn sanity_line_7_resolved_twin_is_allow() {
    assert_allow_both("sanity_7", T7, false);
}

#[test]
fn sanity_line_10_resolved_twin_is_allow() {
    assert_allow_both("sanity_10", T10, true);
}

#[test]
fn sanity_line_34_resolved_twin_is_allow() {
    assert_allow_both("sanity_34", T34, false);
}

#[test]
fn sanity_line_35_resolved_twin_is_allow() {
    assert_allow_both("sanity_35", T35, false);
}

#[test]
fn sanity_line_52_resolved_twin_is_allow() {
    assert_allow_both("sanity_52", T52, false);
}

#[test]
fn sanity_line_53_resolved_twin_is_allow() {
    assert_allow_both("sanity_53", T53, false);
}

#[test]
fn sanity_line_57_resolved_twin_is_allow() {
    assert_allow_both("sanity_57", T57, false);
}

#[test]
fn sanity_line_59_resolved_twin_is_allow() {
    assert_allow_both("sanity_59", T59, false);
}

#[test]
fn sanity_line_66_resolved_twin_is_allow() {
    assert_allow_both("sanity_66", T66, false);
}

#[test]
fn sanity_line_73_resolved_twin_is_allow() {
    // Re-anchored Allow -> Undetermined: user ruling 2026-11-18 (S4 B.5).
    assert_b5_undetermined("sanity_73", T73, false);
}

#[test]
fn sanity_line_99_resolved_twin_is_allow() {
    assert_allow_both("sanity_99", T99, false);
}

#[test]
fn sanity_line_149_resolved_twin_is_allow() {
    // Re-anchored Allow -> Undetermined: user ruling 2026-11-18 (S4 B.5).
    // The twin ends in a direct exec of the scratch-built
    // `@SCRATCH@/tw/debug/blastguard` (not a system dir, not git-tracked).
    // It became visible to B.5 once `. "$HOME/.cargo/env"` stopped
    // discarding the AST state (main 70f8e714c).
    assert_b5_undetermined("sanity_149", T149, false);
}

#[test]
fn sanity_line_152_resolved_twin_is_allow() {
    assert_allow_both("sanity_152", T152, false);
}

#[test]
fn sanity_line_157_resolved_twin_is_allow() {
    assert_allow_both("sanity_157", T157, false);
}

#[test]
fn sanity_line_172_resolved_twin_is_allow() {
    assert_allow_both("sanity_172", T172, false);
}

#[test]
fn sanity_line_195_resolved_twin_is_allow() {
    // Re-anchored Allow -> Undetermined: user ruling 2026-11-18 (S4 B.5).
    assert_b5_undetermined("sanity_195", T195, false);
}

#[test]
fn sanity_line_200_resolved_twin_is_allow() {
    // Re-anchored Allow -> Undetermined: user ruling 2026-11-18 (S4 B.5).
    assert_b5_undetermined("sanity_200", T200, false);
}

#[test]
fn sanity_line_221_resolved_twin_is_allow() {
    assert_allow_both("sanity_221", T221, false);
}

#[test]
fn sanity_line_224_resolved_twin_is_allow() {
    assert_allow_both("sanity_224", T224, false);
}

#[test]
fn sanity_line_227_resolved_twin_is_allow() {
    // Re-anchored Allow -> Undetermined: user ruling 2026-11-18 (S4 B.5).
    assert_b5_undetermined("sanity_227", T227, false);
}

#[test]
fn sanity_line_230_resolved_twin_is_allow() {
    assert_allow_both("sanity_230", T230, false);
}

#[test]
fn sanity_line_250_resolved_twin_is_allow() {
    assert_allow_both("sanity_250", T250, false);
}

#[test]
fn sanity_line_263_resolved_twin_is_allow() {
    // Re-anchored Allow -> Undetermined: user ruling 2026-11-18 (S4 B.5).
    assert_b5_undetermined("sanity_263", T263, false);
}

#[test]
fn sanity_line_265_resolved_twin_is_allow() {
    assert_allow_both("sanity_265", T265, false);
}

#[test]
fn sanity_line_275_resolved_twin_is_allow() {
    assert_allow_both("sanity_275", T275, false);
}

#[test]
fn sanity_line_281_resolved_twin_is_allow() {
    assert_allow_both("sanity_281", T281, false);
}

#[test]
fn sanity_line_292_resolved_twin_is_allow() {
    assert_allow_both("sanity_292", T292, false);
}

#[test]
fn sanity_line_298_resolved_twin_is_allow() {
    assert_allow_both("sanity_298", T298, false);
}

#[test]
fn sanity_line_299_resolved_twin_is_allow() {
    // Re-anchored Allow -> Undetermined: user ruling 2026-11-18 (S4 B.5).
    assert_b5_undetermined("sanity_299", T299, false);
}

#[test]
fn sanity_line_306_resolved_twin_is_allow() {
    assert_allow_both("sanity_306", T306, false);
}

#[test]
fn sanity_line_315_resolved_twin_is_allow() {
    assert_allow_both("sanity_315", T315, false);
}

#[test]
fn sanity_line_322_resolved_twin_is_allow() {
    assert_allow_both("sanity_322", T322, false);
}

#[test]
fn sanity_line_324_resolved_twin_is_allow() {
    assert_allow_both("sanity_324", T324, false);
}

#[test]
fn sanity_line_327_resolved_twin_is_allow() {
    assert_allow_both("sanity_327", T327, false);
}

#[test]
fn sanity_line_329_resolved_twin_is_allow() {
    assert_allow_both("sanity_329", T329, false);
}

#[test]
fn sanity_line_334_resolved_twin_is_allow() {
    assert_allow_both("sanity_334", T334, false);
}

// ===================================================================
// FP TARGETS — ignored until the AST evaluator is wired into the detectors
// ===================================================================

/// Line 2 [var] (2=subagent). Label: `> $D/decomp.routed.json` with D= literal scratchpad path set on the same line; statically resolvable, harmless scratch file
#[test]
#[ignore = "RED: ast-eval line 2"]
fn fp_line_2_d_scratch_var_redirect() {
    assert_allow_both("fp_2", L2, false);
}

/// Line 7 [var] (7=subagent). Label: `> $S/heredoc_base.txt` where S=/private/tmp/.../scratchpad/condukt is assigned literally at the start of the same line
#[test]
#[ignore = "RED: ast-eval line 7"]
fn fp_line_7_s_scratch_var_redirect_after_pipeline() {
    assert_allow_both("fp_7", L7, false);
}

/// Line 10 [var] (10=subagent). Label: `> $S/fix.patch` with S=/private/tmp/.../scratchpad assigned literally on the same line
#[test]
#[ignore = "RED: ast-eval line 10"]
fn fp_line_10_s_scratch_var_git_diff_redirect() {
    assert_allow_both("fp_10", L10, true);
}

/// Line 34 [var] (34=subagent). Label: `cp ... $B/` with B=/private/tmp/.../scratchpad/f70e-bak literal on the same line
#[test]
fn fp_line_34_b_scratch_var_cp_destination() {
    assert_allow_both("fp_34", L34, false);
}

/// Line 35 [for] (35=subagent). Label: `> $S/$t.txt` with S literal scratchpad and t from a literal for-list of test names
#[test]
#[ignore = "RED: ast-eval line 35"]
fn fp_line_35_for_loop_var_in_scratch_redirect() {
    assert_allow_both("fp_35", L35, false);
}

/// Line 52 [var] (52=interactive). Label: `> $S/bl.json` with S=/private/tmp/.../scratchpad literal on same line
#[test]
#[ignore = "RED: ast-eval line 52"]
fn fp_line_52_s_scratch_var_redirect_with_fallback() {
    assert_allow_both("fp_52", L52, false);
}

/// Line 53 [cd] (53=subagent, 54=interactive). Label: `cat > tests/....rs <<'EOF'` after `cd /Users/yuki/src/.harness-worktrees/.../crates/backlog` (literal); relative target resolvable in a worktree
/// Twins de-duplicated into this test: lines 53, 54 (same command).
#[test]
#[ignore = "RED: ast-eval line 53"]
fn fp_line_53_literal_cd_then_relative_heredoc_redirect() {
    assert_allow_both("fp_53", L53, false);
}

/// Line 57 [var] (57=subagent). Label: `sed -i '' ... $f` with f=crates/condukt/src/testscope.rs assigned literally on the same line after literal cd into a worktree
#[test]
fn fp_line_57_f_literal_var_sed_in_place() {
    assert_allow_both("fp_57", L57, false);
}

/// Line 59 [var] (59=subagent). Label: `> $D/decomp.routed.json` with D= literal scratchpad path set on same line
#[test]
#[ignore = "RED: ast-eval line 59"]
fn fp_line_59_d_scratch_var_redirect_run_dir() {
    assert_allow_both("fp_59", L59, false);
}

/// Line 66 [var] (66=subagent). Label: `> $O` with O=/private/tmp/.../scratchpad/out.txt assigned literally on the same line
#[test]
#[ignore = "RED: ast-eval line 66"]
fn fp_line_66_o_scratch_file_var_redirect() {
    assert_allow_both("fp_66", L66, false);
}

/// Line 73 [var] (73=subagent). Label: `cat > $S/c1.json <<'J'` with S literal scratchpad assigned on same line; quoted heredoc JSON data
#[test]
fn fp_line_73_s_scratch_var_quoted_heredoc_redirect() {
    // Re-anchored Allow -> Undetermined: user ruling 2026-11-18 (S4 B.5).
    assert_b5_undetermined("fp_73", L73, false);
}

/// Line 99 [var] (99=subagent). Label: `sed -i '' '94d' $F` with F=crates/blastguard/tests/...rs assigned literally after literal cd; changes were also saved via git diff > fix.patch
#[test]
fn fp_line_99_f_literal_var_sed_in_place_after_git_diff() {
    assert_allow_both("fp_99", L99, false);
}

/// Line 149 [var] (149=subagent). Label: S assigned literal scratchpad and the command word `$S/tw/debug/blastguard` is the just-built project binary, statically derivable
#[test]
#[ignore = "RED: ast-eval line 149"]
fn fp_line_149_s_scratch_var_command_word_built_binary() {
    assert_allow_both("fp_149", L149, false);
}

/// Line 152 [var] (152=subagent). Label: f=backlog_...rs assigned literally on the same line; `sed -i '93,162d' $f` edits a repo test in worktree
#[test]
#[ignore = "RED: ast-eval line 152"]
fn fp_line_152_literal_cd_f_var_sed_in_place() {
    assert_allow_both("fp_152", L152, false);
}

/// Line 157 [var] (157=subagent). Label: f assigned a literal filename on the same line; sed -i in worktree tests dir judged unresolvable
#[test]
#[ignore = "RED: ast-eval line 157"]
fn fp_line_157_literal_cd_f_var_sed_in_place_in_for_body() {
    assert_allow_both("fp_157", L157, false);
}

/// Line 172 [var] (172=subagent). Label: f=crates/blastguard/tests/...rs assigned literally; rm of a repo test file and sed -i on a literal path in worktree
#[test]
#[ignore = "RED: ast-eval line 172"]
fn fp_line_172_f_literal_var_sed_in_place_after_rm() {
    assert_allow_both("fp_172", L172, false);
}

/// Line 195 [var] (195=subagent). Label: P assigned literal scratchpad p.sh on the same line; `chmod +x $P` is on a scratch file
#[test]
fn fp_line_195_p_scratch_var_chmod() {
    // Re-anchored Allow -> Undetermined: user ruling 2026-11-18 (S4 B.5).
    assert_b5_undetermined("fp_195", L195, false);
}

/// Line 200 [var] (200=subagent). Label: S literal scratchpad; `sed -i.bak ... $S/run.py` on scratch file
#[test]
fn fp_line_200_s_scratch_var_sed_in_place() {
    // Re-anchored Allow -> Undetermined: user ruling 2026-11-18 (S4 B.5).
    assert_b5_undetermined("fp_200", L200, false);
}

/// Line 221 [stdin] (221=interactive, 222=interactive, 223=subagent). Label: `echo hi | bash /dev/stdin`: stdin program is the literal `hi`, determinable and harmless
/// Twins de-duplicated into this test: lines 221, 222, 223 (same command).
#[test]
#[ignore = "RED: ast-eval line 221"]
fn fp_line_221_echo_literal_into_bash_dev_stdin() {
    assert_allow_both("fp_221", L221, false);
}

/// Line 224 [stdin] (224=interactive, 225=interactive, 226=subagent). Label: `echo hi | bash`: piped program is literal `hi`, determinable and harmless
/// Twins de-duplicated into this test: lines 224, 225, 226 (same command).
#[test]
#[ignore = "RED: ast-eval line 224"]
fn fp_line_224_echo_literal_into_bash() {
    assert_allow_both("fp_224", L224, false);
}

/// Line 227 [operand] (227=interactive, 228=interactive, 229=subagent). Label: `bash script.sh /dev/stdin`: program is script.sh, /dev/stdin is merely an operand; reason misreads it as stdin program
/// Twins de-duplicated into this test: lines 227, 228, 229 (same command).
#[test]
#[ignore = "RED: ast-eval line 227"]
fn fp_line_227_bash_script_operand_dev_stdin_is_an_arg() {
    assert_allow_both("fp_227", L227, false);
}

/// Line 230 [operand] (230=interactive, 231=interactive, 232=subagent). Label: `bash -c echo /dev/stdin`: program is the literal `echo`; /dev/stdin is $0, harmless
/// Twins de-duplicated into this test: lines 230, 231, 232 (same command).
#[test]
#[ignore = "RED: ast-eval line 230"]
fn fp_line_230_bash_dash_c_word_dev_stdin_is_dollar0() {
    assert_allow_both("fp_230", L230, false);
}

/// Line 250 [operand] (250=interactive, 251=interactive, 252=subagent). Label: `bash -c "echo hi" /dev/stdin`: program is the literal `echo hi`; /dev/stdin is $0
/// Twins de-duplicated into this test: lines 250, 251, 252 (same command).
#[test]
#[ignore = "RED: ast-eval line 250"]
fn fp_line_250_bash_dash_c_quoted_dev_stdin_is_dollar0() {
    assert_allow_both("fp_250", L250, false);
}

/// Line 263 [for] (263=subagent). Label: `> $S/p_$m.json` with S a literal scratchpad path and m looped over literal `main sub`; fully determinable scratch file
#[test]
fn fp_line_263_for_loop_var_in_scratch_redirect_jq() {
    // Re-anchored Allow -> Undetermined: user ruling 2026-11-18 (S4 B.5).
    assert_b5_undetermined("fp_263", L263, false);
}

/// Line 265 [var] (265=interactive). Label: `F=crates/...rs && ... > $F` F is a literal assigned earlier in the && chain (own worktree test file)
#[test]
fn fp_line_265_f_literal_var_redirect_in_group() {
    assert_allow_both("fp_265", L265, false);
}

/// Line 275 [var] (275=subagent, 276=interactive). Label: `git add $T` where T is a single literal path assigned earlier on the line; not whole-tree staging
/// Twins de-duplicated into this test: lines 275, 276 (same command).
#[test]
#[ignore = "RED: ast-eval line 275"]
fn fp_line_275_t_literal_var_git_add() {
    assert_allow_both("fp_275", L275, false);
}

/// Line 281 [for] (281=subagent). Label: `for f in decomp decomp.routed; do jq ... > $D/$f.tmp && mv ...` D literal scratchpad, f from literal list
#[test]
#[ignore = "RED: ast-eval line 281"]
fn fp_line_281_for_loop_var_scratch_tmp_then_mv() {
    assert_allow_both("fp_281", L281, false);
}

/// Line 292 [var] (292=subagent, 294=interactive). Label: `S=/private/tmp/.../scratchpad && mkdir -p $S && cp target/debug/blastguard $S/bg-fix` literal scratchpad destination
/// Twins de-duplicated into this test: lines 292, 294 (same command).
#[test]
fn fp_line_292_s_scratch_var_cp_destination() {
    assert_allow_both("fp_292", L292, false);
}

/// Line 298 [for] (298=subagent). Label: `for f in <4 literal repo files>; do sed -i '' ... "$f"` loop over literal relative paths in a git worktree
#[test]
fn fp_line_298_for_loop_literal_files_sed_in_place() {
    assert_allow_both("fp_298", L298, false);
}

/// Line 299 [for] (299=subagent). Label: `for b in bg-fix bg-base; do ./$b --version` literal list of scratchpad binaries, `--version` only
#[test]
#[ignore = "RED: ast-eval line 299"]
fn fp_line_299_for_loop_command_word_scratch_binaries() {
    assert_allow_both("fp_299", L299, false);
}

/// Line 306 [var] (306=subagent). Label: `S=<scratchpad>; cargo test ... > $S/base_int.txt` literal scratchpad target
#[test]
#[ignore = "RED: ast-eval line 306"]
fn fp_line_306_s_scratch_var_cargo_test_redirect() {
    assert_allow_both("fp_306", L306, false);
}

/// Line 315 [cd] (315=subagent, 328=interactive). Label: `S=<scratchpad>/brushspike && cd $S && cat > Cargo.toml` cd target is a literal scratch path assigned on the line
/// Twins de-duplicated into this test: lines 315, 328 (same command).
#[test]
#[ignore = "RED: ast-eval line 315"]
fn fp_line_315_s_scratch_var_cd_then_relative_heredoc() {
    assert_allow_both("fp_315", L315, false);
}

/// Line 322 [var] (322=subagent). Label: `D=<scratchpad>; ... echo "$RID" > $D/rid` D is a literal assigned on the line
#[test]
#[ignore = "RED: ast-eval line 322"]
fn fp_line_322_d_scratch_var_redirect_rid() {
    assert_allow_both("fp_322", L322, false);
}

/// Line 324 [var] (324=interactive). Label: `S=<scratchpad>/bm && git show :1:... > $S/O` S literal scratchpad path
#[test]
fn fp_line_324_s_scratch_var_git_show_redirect() {
    assert_allow_both("fp_324", L324, false);
}

/// Line 327 [var] (327=subagent). Label: `D=$S/draft` with S literal scratchpad; `> $D/helpers.rs` fully determinable scratch file
#[test]
fn fp_line_327_chained_var_scratch_redirect() {
    assert_allow_both("fp_327", L327, false);
}

/// Line 329 [stdin] (329=subagent). Label: `python3 - <<'EOF' 2>&1 | tail -3` heredoc is on the line (empty body) yet judged as stdin program not on command line
#[test]
#[ignore = "RED: ast-eval line 329"]
fn fp_line_329_python_empty_quoted_heredoc_is_literal_stdin() {
    assert_allow_both("fp_329", L329, false);
}

/// Line 334 [var] (334=interactive). Label: `F=crates/...rs && sed -i '' ... $F` F is a literal relative path in own worktree
#[test]
fn fp_line_334_f_literal_var_sed_in_place_version_bump() {
    assert_allow_both("fp_334", L334, false);
}

/// Line 69 [var] (69=subagent). Label: `> $f` with f=crates/blastguard/tests/....rs assigned literally on same line after literal cd into worktree
///
/// Asserts only that the verdict is NOT caused by the unresolved word
/// ``$f``: resolved twin is still Ask/Deny: `printf` writes the file with `"$python_src"` (a `$(..)` value) and the shell may run it -- a write-then-run rule on an Undetermined value, which stays restrictive by contract.
#[test]
#[ignore = "RED: ast-eval line 69"]
fn fp_line_69_f_literal_var_printf_redirect() {
    assert_not_caused_by("fp_69", L69, r####"`$f`"####);
}

/// Line 112 [var] (112=subagent). Label: `> $S/fix.txt` with S=/private/tmp/.../scratchpad assigned literally earlier on the same line; quoted heredoc Rust test file is data
///
/// Asserts only that the verdict is NOT caused by the unresolved word
/// `$S/fix.txt`: resolved twin is still Ask/Deny on `>> {bt}"`, text INSIDE the quoted `<<'EOF'` Rust body read as a relative redirect -- a heredoc-data FP of the shell-parser migration (tests/shell_parser_differential_corpus_20261115.rs), not this file's class.
#[test]
#[ignore = "RED: ast-eval line 112"]
fn fp_line_112_s_scratch_var_redirect_after_rs_heredoc() {
    assert_not_caused_by("fp_112", L112, r####"$S/fix.txt"####);
}

/// Line 164 [var] (164=subagent). Label: W and D assigned literally; command word `$W/target/debug/blastguard` is the project's own binary, piped output to python3 -c json parse
///
/// Asserts only that the verdict is NOT caused by the unresolved word
/// `$W/target/debug/blastguard`: resolved twin is still Ask/Deny: the `python3 -c "..."` program "cannot be tokenised as Python" -- python-level, out of this file's scope.
#[test]
#[ignore = "RED: ast-eval line 164"]
fn fp_line_164_w_literal_var_command_word_under_env() {
    assert_not_caused_by("fp_164", L164, r####"$W/target/debug/blastguard"####);
}

/// Line 284 [var] (284=subagent). Label: `cat > $S/ws/.../x.rs <<'EOF'` S is literal scratchpad path assigned on the line; body is quoted data
///
/// Asserts only that the verdict is NOT caused by the unresolved word
/// `$S/ws/crates/answerlang/tests/stop_payload_final_message_20261115.rs`: resolved twin is still Ask/Deny on "> file`", text INSIDE the quoted `<<'EOF'` Rust body read as a redirect -- a heredoc-data FP of the shell-parser migration, not this file's class.
#[test]
#[ignore = "RED: ast-eval line 284"]
fn fp_line_284_s_scratch_var_quoted_heredoc_rs_redirect() {
    assert_not_caused_by(
        "fp_284",
        L284,
        r####"$S/ws/crates/answerlang/tests/stop_payload_final_message_20261115.rs"####,
    );
}

// ===================================================================
// CONTROLS — NOT ignored; must stay blocked through the migration. Each pins
// the exact `(main, subagent)` pair observed at the rev of this commit.
// ===================================================================

/// A literal variable that resolves to a system directory: resolving `$S`
/// must make this a refusal, not an Allow.
#[test]
fn control_var_resolving_to_etc_rm_rf() {
    check_control("control_var_etc", "S=/etc; rm -rf $S", "deny", "deny");
}

/// A literal variable that resolves into a system bin dir, as a redirect
/// target.
#[test]
fn control_var_resolving_to_usr_local_bin_redirect() {
    check_control(
        "control_var_usr_local_bin",
        "D=/usr/local/bin; echo x > $D/x",
        "deny",
        "deny",
    );
}

/// A literal for-list in which ONE value is a system directory: per-value
/// instantiation must keep the `/etc` instance.
#[test]
fn control_for_list_with_etc_rm_rf() {
    check_control(
        "control_for_etc",
        "for d in /etc /tmp/a; do rm -rf $d; done",
        "deny",
        "deny",
    );
}

/// A literal stdin program that is destructive: knowing the program text must
/// make this a refusal, not an Allow.
#[test]
fn control_literal_destructive_stdin_program_to_bash() {
    check_control(
        "control_stdin_rm_etc",
        "echo 'rm -rf /etc' | bash",
        "deny",
        "deny",
    );
}

/// An unknown script with `/dev/stdin` as an operand while a destructive
/// literal is piped in: the script may read (and run) its operand. Observed
/// at this rev: deny/deny with the system-harm `rm -rf /etc` reason (the
/// current detector reads `/dev/stdin` as the program). NOTE: the plain
/// `bash x.sh /dev/stdin` (no harmful stdin) is NOT a control here — it is
/// the FP target of line 227 (expected Allow), and its operand-free twin
/// `bash script.sh` is Allow at this rev.
#[test]
fn control_unknown_script_dev_stdin_operand_with_destructive_stdin() {
    check_control(
        "control_unknown_script_dev_stdin",
        "echo 'rm -rf /etc' | bash x.sh /dev/stdin",
        "deny",
        "deny",
    );
}
