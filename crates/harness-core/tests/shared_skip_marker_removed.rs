// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! MIRROR-GAP GUARD: the shared project-root skip marker must be gone from
//! EVERY gate, not just from the one that was fixed first.
//!
//! Four Stop gates (donegate, propguard, reviewgate, tdd) consumed four
//! project-root marker files through one shared primitive
//! (`gate::run::consume_skip(&root, ".<gate>-skip")`). The project root is
//! shared by every concurrent session and the marker names no session, so
//! whichever session's Stop hook fires next consumes it — its own legitimate
//! gate waved through on an exception it never asked for. CLAUDE.md §5 forbids
//! precisely this.
//!
//! This repo's recurring failure mode is that a fix lands on ONE of several
//! copies of such a path and the audit reads as converged. donegate's own
//! end-to-end suite stays green while three shared hatches remain live. These
//! two tests are the cheap, total guard against that.
//!
//! Kept in its own file (rather than beside the behavioural tests in
//! `session_scoped_skip.rs`) so it compiles and runs — and could be observed
//! RED — against a tree where the replacement primitive does not exist yet.
//!
//! ## Adjudication: the doc needle measures ADVERTISING, not MENTIONING
//!
//! The first version of `no_operator_facing_doc_still_advertises_the_shared_marker`
//! flagged any line containing a marker name. Its name, its failure message and
//! its docstring all claimed a narrower property — "tells a human to create a
//! file that does nothing" — so the needle and its own stated contract
//! disagreed. That is a docstring-vs-behaviour mismatch (CLAUDE.md §4) inside a
//! test, and the prose is the half that was right: a line recording that the
//! marker was REMOVED does not instruct anybody to create one.
//!
//! It was narrowed rather than the docs being stripped, because the name
//! outlives the file. `.donegate-skip` and friends survive in transcripts,
//! session records, old commit messages, `docs/article-llm-fail-open.md` and
//! CLAUDE.md §5 — which cites the marker as its worked example of the forbidden
//! pattern. A reader who greps for it must land on "removed, and here is why";
//! deleting the name would make the docs silent exactly where §4 requires them
//! to be explicit. "No trace anywhere" was also never enforceable: two of those
//! sources are already out of scope, so the stricter reading would only have
//! been applied to the places this scan happens to reach.
//!
//! Retuning a test after seeing the implementation is the asymmetry CLAUDE.md §6
//! warns about — a critique accepted without the scrutiny a finding would get.
//! Three things hold it in check. The new predicate carries NO removal-keyword
//! exemption, which was the tailored fix available and was rejected; it is a
//! conjunction of two independent conditions, so it fails toward flagging; and
//! `the_advertising_needle_flags_a_re_advertising_line` measures its kill rate
//! against synthetic re-advertising lines through the same scanner, so a needle
//! weakened until the repo passes stops being able to see an advertisement at
//! all. Checked before the rule was written, not after: none of the 14 lines
//! the old needle flagged contains a creation verb, so the rule was not shaped
//! around them.
//!
//! Written by a DISINTERESTED author (CLAUDE.md §2(a)).
//!
//! ## Isolation
//!
//! Read-only: these tests only read files from the checkout they are compiled
//! in. They touch no `$HOME` state and mutate nothing.

use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------

/// Repo root: the nearest ancestor containing `.git` (a file in a linked
/// worktree, a directory in the main checkout — `exists()` covers both).
fn repo_root() -> Option<PathBuf> {
    let mut cur = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        if cur.join(".git").exists() {
            return Some(cur);
        }
        if !cur.pop() {
            return None;
        }
    }
}

/// Every `*.rs` under `crates/*/src/`.
fn gate_sources(root: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    let Ok(crates) = std::fs::read_dir(root.join("crates")) else {
        return out;
    };
    for c in crates.flatten() {
        walk(&c.path().join("src"), &mut out);
    }
    out.sort();
    out
}

const SHARED_MARKERS: [&str; 4] = [
    ".donegate-skip",
    ".propguard-skip",
    ".reviewgate-skip",
    ".tdd-skip",
];

/// The four gates whose shared markers are in scope, plus the shared primitive
/// they consumed them through.
const GATE_CRATES: [&str; 5] = ["donegate", "propguard", "reviewgate", "tdd", "harness-core"];

/// True for a line that is nothing but a `//`, `///` or `//!` comment.
///
/// Excluded on purpose: an explanation of what the marker USED to be ("Replaces
/// the old `.donegate-skip` file in the project root, which sat in the shared
/// tree…") is exactly the history a future reader needs. What must not survive
/// is a live read of the file, or block-reason prose telling an operator to
/// create one — and both of those live in code and string literals, which this
/// filter keeps.
fn is_comment_only(line: &str) -> bool {
    line.trim_start().starts_with("//")
}

/// This repo's recurring failure mode is the MIRROR GAP: a fix lands on one of
/// several copies of the same path and the audit reads as converged. Four gates
/// consumed four shared markers through one shared primitive, so a fix that
/// only rewires donegate leaves three live shared hatches — and every
/// end-to-end test that only drives donegate would still be green.
///
/// A source-level assertion is used deliberately: the property is "no code path
/// anywhere still reads a project-root marker", and enumerating that
/// behaviourally would require standing up all four gates in a blocking state.
/// It is a coarse instrument and it is stated as such — it proves the read is
/// gone, not that the replacement is correct. The behavioural tests in
/// `session_scoped_skip.rs` and the per-gate end-to-end suites carry that
/// burden.
#[test]
fn no_gate_crate_still_reads_a_shared_project_root_skip_marker() {
    let Some(root) = repo_root() else {
        panic!(
            "cannot locate the repo root — this test cannot be run meaningfully outside a checkout"
        );
    };
    let mut hits: Vec<String> = Vec::new();
    for path in gate_sources(&root) {
        let in_scope = GATE_CRATES.iter().any(|c| {
            path.components()
                .any(|comp| comp.as_os_str().to_string_lossy() == **c)
        });
        if !in_scope {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (i, line) in text.lines().enumerate() {
            if is_comment_only(line) {
                continue;
            }
            if SHARED_MARKERS.iter().any(|m| line.contains(m)) || line.contains("consume_skip(") {
                hits.push(format!("{}:{}: {}", path.display(), i + 1, line.trim()));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "a shared project-root skip marker is still reachable from gate-crate source. Every one \
         of these is a hatch that one session can create and a DIFFERENT session's Stop hook will \
         consume (CLAUDE.md §5). Block-reason prose counts too: telling an operator to create a \
         file that no longer works is a docstring-vs-behaviour lie (CLAUDE.md §4).\n{}",
        hits.join("\n")
    );
}

/// A FIFTH instance of the same defect, found by widening the scan beyond the
/// four Stop gates: `precommit-audit` consumes `<root>/<audit_dir>/.audit-skip`
/// (`crates/precommit-audit/src/hookio.rs`, `consume_skip`), whose default
/// `audit_dir` is `.claude` — i.e. a one-shot, unattributed file inside the
/// SHARED project tree, deleted by whichever invocation reads it first. It is
/// the identical mechanism CLAUDE.md §5 forbids, in a crate the session-scoped
/// rework was not scoped to.
///
/// This test is expected to be RED until precommit-audit's hatch is
/// session-scoped too (or the item is consciously deferred and this test
/// retargeted). It is kept rather than quietly narrowed away, because leaving a
/// known instance undetected to keep a suite green is the one thing CLAUDE.md
/// §4 forbids outright. Its own test so its verdict cannot be confused with the
/// four-gate scope above.
#[test]
fn no_other_crate_consumes_an_unattributed_shared_skip_file() {
    let Some(root) = repo_root() else {
        panic!(
            "cannot locate the repo root — this test cannot be run meaningfully outside a checkout"
        );
    };
    let mut hits: Vec<String> = Vec::new();
    for path in gate_sources(&root) {
        let in_gate_scope = GATE_CRATES.iter().any(|c| {
            path.components()
                .any(|comp| comp.as_os_str().to_string_lossy() == **c)
        });
        if in_gate_scope {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (i, line) in text.lines().enumerate() {
            if is_comment_only(line) {
                continue;
            }
            if line.contains(".audit-skip") || line.contains("fn consume_skip(") {
                hits.push(format!("{}:{}: {}", path.display(), i + 1, line.trim()));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "another crate still consumes a one-shot, unattributed skip file from the shared project \
         tree — the same mechanism, outside the four Stop gates. One session creates it and \
         whichever invocation runs next consumes it (CLAUDE.md §5).\n{}",
        hits.join("\n")
    );
}

/// Verbs that turn a MENTION of the marker into an INSTRUCTION to create one.
///
/// Advertising a file hatch requires telling the reader to bring the file into
/// existence, so this list is the operative half of the predicate. Both
/// languages the docs are written in are covered, because a rule that only
/// caught the English half would let the `.ja.md` mirror re-advertise freely —
/// the same mirror gap, one directory over.
const CREATE_VERBS: [&str; 14] = [
    "create",
    "creating",
    "touch ",
    "echo ",
    "put ",
    "place ",
    "drop ",
    "write a",
    "作る",
    "作成",
    "置く",
    "置い",
    "用意",
    "書き込",
];

/// True when `line` tells a reader to CREATE one of the removed markers.
///
/// The predicate is deliberately a conjunction of two independent conditions —
/// the marker name AND a creation verb — and carries no exemption keyword. An
/// earlier version of this test flagged the marker name alone, which is a
/// different (and wider) property than the one its own name, message and
/// docstring claimed; see the module docs for that adjudication. Note what is
/// NOT here: there is no "removed"/"廃止"/"no longer" escape word. Adding one
/// would let `create .tdd-skip (this replaces X)` through, and would be a rule
/// shaped around the sentences that happen to exist today rather than around
/// the property.
///
/// Known limit, stated rather than hidden: a removal notice that happens to use
/// a creation verb ("the `.tdd-skip` file you used to create is gone") is a
/// false positive. That is the conservative direction — it fails toward
/// flagging — and the fix is to write the sentence without the verb.
fn advertises_marker(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    SHARED_MARKERS.iter().any(|m| line.contains(m))
        && CREATE_VERBS.iter().any(|v| lower.contains(v))
}

/// Every operator-facing markdown file in scope.
fn operator_docs(root: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "md") {
                out.push(p);
            }
        }
    }
    let mut docs = Vec::new();
    if let Ok(crates) = std::fs::read_dir(root.join("crates")) {
        for c in crates.flatten() {
            for sub in ["README.md", "README.ja.md"] {
                let p = c.path().join(sub);
                if p.is_file() {
                    docs.push(p);
                }
            }
            walk(&c.path().join("skills"), &mut docs);
        }
    }
    walk(&root.join("docs/specs"), &mut docs);
    docs.sort();
    docs
}

/// Scan `docs` and return every `path:line: text` that advertises a marker.
/// Shared by the repo-wide test and by the fixture test that measures this
/// scanner's own kill rate, so the two can never drift apart.
fn advertising_hits(docs: &[PathBuf]) -> Vec<String> {
    let mut hits = Vec::new();
    for path in docs {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        for (i, line) in text.lines().enumerate() {
            if advertises_marker(line) {
                hits.push(format!("{}:{}: {}", path.display(), i + 1, line.trim()));
            }
        }
    }
    hits
}

/// The operator-facing instructions are part of the hatch. A README or SKILL
/// that still tells a human to drop `.donegate-skip` in the project root is
/// advertising a mechanism that either (a) still exists, in which case the fix
/// is incomplete, or (b) no longer exists, in which case the docs now lie.
/// Either way it fails. Historical narrative (`docs/article-*.md`) and the repo
/// CLAUDE.md — which cites the marker as the example of what NOT to do — are
/// deliberately out of scope.
///
/// A line recording that the marker WAS REMOVED is not a hit, and must not be:
/// the name will outlive the file in transcripts, session records, old commit
/// messages and CLAUDE.md §5 itself, so a reader who greps for `.tdd-skip` has
/// to land on "removed, and here is why". Deleting the name would make the docs
/// silent exactly where CLAUDE.md §4 requires them to be explicit.
#[test]
fn no_operator_facing_doc_still_advertises_the_shared_marker() {
    let Some(root) = repo_root() else {
        panic!(
            "cannot locate the repo root — this test cannot be run meaningfully outside a checkout"
        );
    };
    let docs = operator_docs(&root);
    assert!(
        !docs.is_empty(),
        "apparatus: no operator-facing docs were found at all, so an empty hit list would prove \
         nothing"
    );
    let hits = advertising_hits(&docs);
    assert!(
        hits.is_empty(),
        "operator-facing documentation still tells a reader to CREATE the removed shared \
         project-root skip marker:\n{}",
        hits.join("\n")
    );
}

/// KILL-RATE CONTROL for the needle above, and the reason this file may narrow
/// what it measures without that narrowing being a way to make the docs pass.
///
/// The scanner is pointed at synthetic fixture files: one carrying re-advertising
/// lines in every shape the real docs use (English and Japanese, prose and
/// bullet), one carrying the removal-notice lines that must NOT be flagged. The
/// same `advertising_hits` the repo-wide test uses does the scanning, so a
/// needle weakened until the repo passes would be caught here — every positive
/// fixture would stop being flagged.
#[test]
fn the_advertising_needle_flags_a_re_advertising_line() {
    let dir = std::env::temp_dir().join(format!("skip-needle-fixture-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create fixture dir");

    // MUST be flagged: each of these instructs a reader to bring the file into
    // existence, which is the property the test claims to measure.
    let advertising = [
        "- **Escape hatch**: create `.donegate-skip` (one-line reason) in the project root.",
        "One-shot: create `.reviewgate-skip` in the project root (a one-line reason);",
        "Escape hatches: create `.propguard-skip` (one-shot, with a one-line reason) or",
        "  touch .tdd-skip && echo \"refactor only\" > .tdd-skip",
        "Just drop a `.tdd-skip` file at the repo root to get past this.",
        "- **エスケープハッチ**: プロジェクトルートに `.donegate-skip`（1 行の理由）を作ると通る。",
        "リファクタのみなら、プロジェクト直下に `.tdd-skip` を置く。",
        "`.reviewgate-skip` を作成すれば次の stop は許可される。",
    ];
    let ad_path = dir.join("READMEadvertising.md");
    std::fs::write(&ad_path, advertising.join("\n")).expect("write fixture");

    let flagged = advertising_hits(std::slice::from_ref(&ad_path));
    assert_eq!(
        flagged.len(),
        advertising.len(),
        "the needle missed a re-advertising line. A needle that cannot see an instruction to \
         create the marker is not measuring advertisement at all — it has been tuned until the \
         current docs pass. flagged={flagged:#?}"
    );

    // MUST NOT be flagged: verbatim removal notices taken from the docs as
    // written. These record that the file is gone; none of them tells anyone to
    // make one. Flagging them is what the previous needle did, and is the
    // over-match this test's contract was retuned to exclude.
    let removal_notices = [
        "`.tdd-skip` file in the project root was removed: a one-shot marker in a shared",
        "`.reviewgate-skip` file in the project root was removed — a one-shot marker in",
        "so a bypass cannot happen unrecorded. (This replaces the old `.donegate-skip`",
        "共有 project root の `.propguard-skip` は撤去済み（CLAUDE.md §5）。",
        "旧来の project root の `.tdd-skip` ファイルは撤去した — 共有ツリーの 1 回限りマーカーは次に停止したセッションが消費してしまう。",
    ];
    let ok_path = dir.join("READMEremoval.md");
    std::fs::write(&ok_path, removal_notices.join("\n")).expect("write fixture");

    let false_positives = advertising_hits(&[ok_path]);
    assert!(
        false_positives.is_empty(),
        "the needle flagged a line that merely records the marker's REMOVAL. Those lines are the \
         §4 obligation, not a violation of it — the name outlives the file in transcripts and old \
         commits, so a reader who greps for it must land on 'removed, and here is why'.\n{}",
        false_positives.join("\n")
    );
}
