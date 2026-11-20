//! Contract tests for the /flow skill prose (`skills/flow/SKILL.md`, the READMEs)
//! and the plugin layout, pinned to the user rulings of 2026-11-21 (verbatim):
//!
//! - 「flowは問題解決がおわるまで止まることがないようにすること。問題解決とは
//!   backlogなりissuesなりがなくなるまですすめること。」
//! - 「課題を指定されたときは、その課題が実現、解決されるところまですすむこと」
//! - Stop reasons kept: ONLY the circuit breaker (`condukt circuit check` trip).
//!   Removed: budget over/undetermined as an early exit, the compaction handoff,
//!   human-judgment waits.
//! - 「破壊はdeny,未測定はreject,autoで自答はいいが解決できないときは別の問いか
//!   cancel, 基本はすべてAllow」 — every /flow human gate goes through
//!   `condukt policy answer --resolve`, and `condukt circuit check` is called with
//!   `--no-compaction-axis`.
//!
//! Written by an independent test author (not the implementer), CLAUDE.md §2(a).
//!
//! # Robustness
//!
//! Command assertions are made on fenced code blocks (``` ... ```), with
//! backslash-continued lines joined and a trailing `  # comment` dropped, so a
//! command mentioned only in prose does not satisfy (or break) them. Prose
//! assertions use stable anchors: section headings (`### Step 0`, `### Step 0.5`,
//! `#### 3-1.`) and fixed phrases named by the spec.
//!
//! # What these tests do NOT prove
//!
//! They pin the text the LLM driver reads; they do not run /flow. That the
//! binaries behave as the prose says is covered elsewhere (`tests/stop_gate.rs`,
//! condukt's policy/circuit tests, `scripts/test_flow_snippets_{a,b}.py`).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(p: &Path) -> String {
    fs::read_to_string(p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

fn skill() -> String {
    read(&crate_dir().join("skills/flow/SKILL.md"))
}

/// A fenced block: info-string language, body lines, and the 0-based line
/// index of the opening fence in the source text.
struct Fence {
    #[allow(dead_code)]
    lang: String,
    body: Vec<String>,
    start_line: usize,
}

/// Every ``` fenced block in `text` (indentation before the fence tolerated).
/// An unterminated fence is a test failure, not silently ignored.
fn fences(text: &str) -> Vec<Fence> {
    let mut out = Vec::new();
    let mut cur: Option<Fence> = None;
    for (i, line) in text.lines().enumerate() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("```") {
            match cur.take() {
                Some(f) => out.push(f),
                None => {
                    cur = Some(Fence {
                        lang: rest.trim().to_string(),
                        body: Vec::new(),
                        start_line: i,
                    })
                }
            }
            continue;
        }
        if let Some(f) = cur.as_mut() {
            f.body.push(line.to_string());
        }
    }
    assert!(cur.is_none(), "unterminated fenced block in text");
    out
}

/// Logical command lines of a fence: `\`-continued lines joined with a space,
/// a trailing `  # comment` (two+ spaces then `#`) dropped, whitespace trimmed.
fn logical_lines(f: &Fence) -> Vec<String> {
    let mut out = Vec::new();
    let mut acc = String::new();
    for raw in &f.body {
        let l = raw.trim();
        if let Some(head) = l.strip_suffix('\\') {
            acc.push_str(head.trim_end());
            acc.push(' ');
            continue;
        }
        acc.push_str(l);
        out.push(strip_comment(&acc).trim().to_string());
        acc.clear();
    }
    if !acc.is_empty() {
        out.push(strip_comment(&acc).trim().to_string());
    }
    out
}

/// Drop a trailing shell comment introduced by two-or-more spaces then `#`
/// (the SKILL's convention, e.g. `cmd   # note`). A line that IS a comment
/// becomes empty.
fn strip_comment(l: &str) -> String {
    if l.trim_start().starts_with('#') {
        return String::new();
    }
    let b = l.as_bytes();
    for i in 0..b.len() {
        if b[i] == b'#' && i >= 2 && b[i - 1] == b' ' && b[i - 2] == b' ' {
            return l[..i].to_string();
        }
    }
    l.to_string()
}

/// All logical command lines across every fence in `text` containing `needle`.
fn fenced_cmds(text: &str, needle: &str) -> Vec<String> {
    fences(text)
        .iter()
        .flat_map(logical_lines)
        .filter(|l| l.contains(needle))
        .collect()
}

/// Section from the heading line accepted by `is_start` up to (not including)
/// the next line starting with `stop_prefix`, or a heading of strictly higher
/// level than `level` (`#` count). Panics if the heading is absent.
fn section(text: &str, is_start: impl Fn(&str) -> bool, stop_prefix: &str, level: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.iter().position(|l| is_start(l)).unwrap_or_else(|| {
        panic!("SKILL.md has no section heading for stop_prefix {stop_prefix:?}")
    });
    let mut out = vec![lines[start]];
    for l in &lines[start + 1..] {
        if l.starts_with(stop_prefix) {
            break;
        }
        let hashes = l.chars().take_while(|c| *c == '#').count();
        if hashes > 0 && hashes < level && l[hashes..].starts_with(' ') {
            break;
        }
        out.push(l);
    }
    out.join("\n")
}

/// `### Step 0` (not `### Step 0.5`).
fn step0(text: &str) -> String {
    section(
        text,
        |l| {
            l.strip_prefix("### Step 0").is_some_and(|r| {
                !r.starts_with('.') && !r.starts_with(|c: char| c.is_ascii_digit())
            })
        },
        "### ",
        3,
    )
}

fn step0_5(text: &str) -> String {
    section(text, |l| l.starts_with("### Step 0.5"), "### ", 3)
}

/// `#### 3-1.` up to the next `#### ` (so `#### 3-1.5.` ends it).
fn step3_1(text: &str) -> String {
    section(text, |l| l.starts_with("#### 3-1."), "#### ", 4)
}

/// Paragraphs: runs of non-blank lines.
fn paragraphs(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    for l in text.lines() {
        if l.trim().is_empty() {
            if !cur.is_empty() {
                out.push(cur.join("\n"));
                cur.clear();
            }
        } else {
            cur.push(l);
        }
    }
    if !cur.is_empty() {
        out.push(cur.join("\n"));
    }
    out
}

/// Sentences: each paragraph's lines joined, then split on `。`.
fn sentences(text: &str) -> Vec<String> {
    paragraphs(text)
        .iter()
        .flat_map(|p| {
            p.lines()
                .map(str::trim)
                .collect::<Vec<_>>()
                .join("")
                .split('。')
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Backtick inline-code spans outside fenced blocks.
fn inline_spans(text: &str) -> Vec<String> {
    let fenced: std::collections::HashSet<usize> = fences(text)
        .iter()
        .flat_map(|f| f.start_line..=f.start_line + f.body.len() + 1)
        .collect();
    let mut out = Vec::new();
    for (i, l) in text.lines().enumerate() {
        if fenced.contains(&i) {
            continue;
        }
        let parts: Vec<&str> = l.split('`').collect();
        for (k, p) in parts.iter().enumerate() {
            if k % 2 == 1 && k + 1 < parts.len() {
                out.push((*p).to_string());
            }
        }
    }
    out
}

// ---------------------------------------------------------------- A. Step 0

#[test]
fn a_step0_no_single_item_exit() {
    let s = skill();
    assert!(
        !s.contains("1件だけ実行して終了"),
        "SKILL.md still says to run one item and exit (「1件だけ実行して終了」)"
    );
    assert!(
        !s.contains("ループはせず"),
        "SKILL.md still says 「ループはせず」"
    );
}

#[test]
fn a_step0_argument_task_pursued_until_solved_and_verified() {
    let sec = step0(&skill());
    for w in ["解決", "検証"] {
        assert!(
            sec.contains(w),
            "Step 0 must say an argument task is pursued until realized/solved AND verified; missing {w:?} in:\n{sec}"
        );
    }
}

#[test]
fn a_step0_drain_backlog_argument_loops_until_empty() {
    let sec = step0(&skill());
    for w in ["backlog", "空"] {
        assert!(
            sec.contains(w),
            "Step 0 must say an argument asking to drain the backlog loops until pending is empty; missing {w:?} in:\n{sec}"
        );
    }
    // Section-level co-occurrence alone is vacuous on the pre-ruling text
    // (OBSERVED: 「引数が空なら Step 1」 + an unrelated `backlog add` made it pass),
    // so the drain rule must be stated in ONE sentence naming both backlog and 空,
    // and that sentence must not be the old empty-argument routing line.
    let hits: Vec<String> = sentences(&sec)
        .into_iter()
        .filter(|x| x.contains("backlog") && x.contains("空") && !x.contains("引数が空"))
        .collect();
    assert!(
        !hits.is_empty(),
        "Step 0 has no sentence saying a drain-the-backlog argument loops until the backlog is empty:\n{sec}"
    );
}

// ------------------------------------------------- B. backlog sync before pick

#[test]
fn b_step3_1_syncs_github_issues_before_the_pick() {
    let sec = step3_1(&skill());
    let fs = fences(&sec);
    let pos = |needle: &str| {
        fs.iter()
            .position(|f| logical_lines(f).iter().any(|l| l.starts_with(needle)))
    };
    let sync = pos("backlog sync");
    let pick = pos("backlog next --claim");
    assert!(
        sync.is_some(),
        "Step 3-1 has no fenced `backlog sync` command (GitHub issues mirror):\n{sec}"
    );
    assert!(
        pick.is_some(),
        "Step 3-1 has no fenced `backlog next --claim` pick:\n{sec}"
    );
    assert!(
        sync < pick,
        "fenced `backlog sync` (block #{sync:?}) must come before the pick `backlog next --claim` (block #{pick:?})"
    );
}

// ------------------------------------------------------ C. budget is not an exit

#[test]
fn c_budget_is_not_an_exit() {
    let s = skill();
    assert!(
        !s.contains("予算超過とみなし"),
        "SKILL.md still contains 「予算超過とみなし」"
    );
    let exitish = ["Step 4", "ループ終了", "早期脱出"];
    let hit = |u: &str| u.contains("budgetguard") && exitish.iter().any(|e| u.contains(e));
    let bad_lines: Vec<&str> = s.lines().filter(|l| hit(l)).collect();
    assert!(
        bad_lines.is_empty(),
        "lines tie budgetguard to an exit (Step 4 / ループ終了 / 早期脱出):\n{}",
        bad_lines.join("\n")
    );
    let bad_sentences: Vec<String> = sentences(&s).into_iter().filter(|x| hit(x)).collect();
    assert!(
        bad_sentences.is_empty(),
        "sentences tie budgetguard to an exit:\n{}",
        bad_sentences.join("\n")
    );
}

// ------------------------------------------------- D. compaction is not a stop

#[test]
fn d_no_compaction_stop() {
    let s = skill();
    assert!(
        !s.contains("session_compacted"),
        "SKILL.md still mentions session_compacted"
    );
    assert!(
        !s.contains("--max-compactions"),
        "SKILL.md still mentions --max-compactions"
    );
}

#[test]
fn d_every_fenced_circuit_check_disables_compaction_axis() {
    let s = skill();
    let cmds = fenced_cmds(&s, "condukt circuit check");
    assert!(
        !cmds.is_empty(),
        "SKILL.md has no fenced `condukt circuit check`"
    );
    for c in &cmds {
        for flag in [
            "--no-compaction-axis",
            r#"--run "flow-$CLAUDE_CODE_SESSION_ID""#,
            r#"--session "$CLAUDE_CODE_SESSION_ID""#,
        ] {
            assert!(c.contains(flag), "fenced circuit check lacks {flag}: {c}");
        }
    }
    // Tightening beyond the fenced rule: a prose inline span that spells out an
    // invocation (`condukt circuit check --...`) must not omit the flag either,
    // since the driver can copy it as written.
    for span in inline_spans(&s) {
        if span.trim_start().starts_with("condukt circuit check --") {
            assert!(
                span.contains("--no-compaction-axis"),
                "inline `condukt circuit check` invocation lacks --no-compaction-axis: `{span}`"
            );
        }
    }
}

// ------------------------------------------------------ E. only stop reason

#[test]
fn e_circuit_is_the_only_stop_reason() {
    let s = skill();
    let ps: Vec<String> = paragraphs(&s)
        .into_iter()
        .filter(|p| p.contains("唯一の停止理由"))
        .collect();
    assert!(!ps.is_empty(), "SKILL.md does not state 「唯一の停止理由」");
    for p in &ps {
        assert!(
            p.contains("circuit"),
            "paragraph stating 「唯一の停止理由」 does not name the circuit breaker:\n{p}"
        );
    }
}

// ------------------------------------------------------------------ F. gates

#[test]
fn f_every_fenced_policy_answer_resolves() {
    let s = skill();
    let cmds = fenced_cmds(&s, "condukt policy answer");
    assert!(
        !cmds.is_empty(),
        "SKILL.md has no fenced `condukt policy answer`"
    );
    let bad: Vec<&String> = cmds.iter().filter(|c| !c.contains("--resolve")).collect();
    assert!(
        bad.is_empty(),
        "fenced policy answer without --resolve:\n{bad:#?}"
    );
    let untestable: Vec<&String> = cmds
        .iter()
        .filter(|c| c.contains("--untestable") && !c.contains("--resolve"))
        .collect();
    assert!(
        untestable.is_empty(),
        "--untestable without --resolve:\n{untestable:#?}"
    );
}

#[test]
fn f_step0_5_names_all_four_outcomes() {
    let sec = step0_5(&skill());
    for w in ["deny", "reject", "cancel", "auto"] {
        assert!(sec.contains(w), "Step 0.5 does not mention outcome {w:?}");
    }
}

/// Exception rule (chosen by the test author, as the spec allows): a line that
/// pairs a stop word (`停止` / `止め`) with a gate keyword is an instruction to
/// stop on that gate — UNLESS the same line also contains `止めない` or
/// `止まらない`, which marks it as the explicit negation ("do not stop on X").
/// The keyword list is the spec's (`blocked`, `pivot`, `divert`, `carve`,
/// `彫り直`) widened to `彫` so 「彫り切れなかった…停止報告」/「彫っても…止め」
/// (charter-carve stops that avoid the exact 彫り直 spelling) are also caught.
#[test]
fn f_no_stop_on_blocked_pivot_divert_carve() {
    let s = skill();
    let stop_words = ["停止", "止め"];
    let gate_words = ["blocked", "pivot", "divert", "carve", "彫"];
    let negations = ["止めない", "止まらない"];
    let bad: Vec<&str> = s
        .lines()
        .filter(|l| {
            stop_words.iter().any(|w| l.contains(w))
                && gate_words.iter().any(|w| l.contains(w))
                && !negations.iter().any(|w| l.contains(w))
        })
        .collect();
    assert!(
        bad.is_empty(),
        "SKILL.md still instructs stopping on worker blocked / pivot / spec-gap divert / charter carve:\n{}",
        bad.join("\n")
    );
}

// ------------------------------------------------------- G. Stop hook documented

#[test]
fn g_stop_gate_documented() {
    assert!(
        skill().contains("flow stop-gate"),
        "SKILL.md does not mention `flow stop-gate`"
    );
    for f in ["README.md", "README.ja.md"] {
        let t = read(&crate_dir().join(f));
        assert!(t.contains("stop-gate"), "{f} does not mention stop-gate");
    }
}

// ------------------------------------------------------------------ H. layout

/// `key = "value"` inside the `[package]` table of a Cargo.toml.
fn package_field(toml: &str, key: &str) -> Option<String> {
    let mut in_pkg = false;
    for l in toml.lines() {
        let t = l.trim();
        if t.starts_with('[') {
            in_pkg = t == "[package]";
            continue;
        }
        if !in_pkg {
            continue;
        }
        if let Some((k, v)) = t.split_once('=') {
            if k.trim() == key {
                return Some(v.trim().trim_matches('"').to_string());
            }
        }
    }
    None
}

#[test]
fn h_layout_and_version_lockstep() {
    let cargo = crate_dir().join("Cargo.toml");
    assert!(cargo.is_file(), "crates/flow/Cargo.toml missing");
    let toml = read(&cargo);
    assert_eq!(package_field(&toml, "name").as_deref(), Some("flow"));
    let cv = package_field(&toml, "version").expect("[package].version");

    let pj: serde_json::Value =
        serde_json::from_str(&read(&crate_dir().join(".claude-plugin/plugin.json"))).unwrap();
    assert_eq!(pj["name"], "flow");
    let pv = pj["version"]
        .as_str()
        .expect("plugin.json version")
        .to_string();

    let mp: serde_json::Value = serde_json::from_str(&read(
        &crate_dir().join("../../.claude-plugin/marketplace.json"),
    ))
    .unwrap();
    let entries: Vec<&serde_json::Value> = mp["plugins"]
        .as_array()
        .expect("marketplace plugins[]")
        .iter()
        .filter(|p| p["name"] == "flow")
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "exactly one flow entry in marketplace.json"
    );
    let mv = entries[0]["version"]
        .as_str()
        .expect("marketplace flow version")
        .to_string();

    assert_eq!(
        (&pv, &mv),
        (&cv, &cv),
        "plugin.json / marketplace.json / Cargo.toml versions differ"
    );
}

// ------------------------------------------------- self-tests of the helpers

#[test]
fn helpers_join_continuations_and_strip_comments() {
    let t = "x\n```bash\nOUT=$(condukt policy answer \\\n   --risk low --resolve)   # c\n# only comment\n```\n";
    let cmds = fenced_cmds(t, "condukt policy answer");
    assert_eq!(
        cmds,
        vec!["OUT=$(condukt policy answer --risk low --resolve)".to_string()]
    );
    let t2 = "### Step 0 — a\nA\n### Step 0.5 — b\nB\n";
    assert_eq!(step0(t2), "### Step 0 — a\nA");
    assert_eq!(step0_5(t2), "### Step 0.5 — b\nB");
    let t3 = "#### 3-1. x\n1\n#### 3-1.5. y\n2\n";
    assert_eq!(step3_1(t3), "#### 3-1. x\n1");
    assert_eq!(
        inline_spans("a `b c` d\n```\n`e`\n```\n"),
        vec!["b c".to_string()]
    );
}
