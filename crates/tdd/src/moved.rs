//! Moved-code recognition for the tdd gate: which added implementation lines
//! are code that MOVED (not new) and which are not implementation at all.
//! Pure (no git); [`crate::gate::classify_with_removed`] calls [`count`].
//!
//! Not counted as added implementation lines:
//!
//! * **Moved lines.** An added impl line that is part of a RUN: a maximal
//!   sequence of at least [`MIN_MOVED_RUN`] consecutive added impl lines of
//!   one file (one hunk, see `AddedLine::block`) whose [`move_key`]s equal,
//!   in the same order, consecutive lines REMOVED from one impl file (one
//!   hunk) in the same change set. Multiset: each removed line cancels at
//!   most one added line. A single coincidental line (or two) never cancels.
//!   Removals from non-impl files (docs, test files) never cancel — text
//!   deleted from a note is not code that moved. Indentation is ignored only
//!   in brace languages ([`move_key`]); elsewhere a dedent is a change.
//! * **`use` / `mod` declarations.** A line that is only a `use ...;`
//!   statement (including every line of a multi-line `use` tree) or a
//!   `mod name;` declaration. They (and blank lines) are skipped
//!   transparently: they neither count toward a run nor break one.
//!
//! Limits, stated: a block of 3+ lines moved to a different place in its
//! file still reads as a move (reordering whole blocks can change behaviour);
//! a run is matched on text only, so 3+ lines of boilerplate deleted
//! somewhere can vouch for the same 3+ lines added elsewhere.

use std::collections::BTreeSet;
use std::path::Path;

use harness_core::verdict::Determination;
use regex::RegexSet;

use crate::gate::Report;
use crate::git::{AddedLine, AddedScan};

/// What [`count`] found in the added lines.
pub struct Counted {
    /// Added implementation lines that are not moved and not `use`/`mod`.
    pub added_impl_lines: usize,
    /// Added implementation lines read as moved (part of a run).
    pub moved_impl_lines: usize,
    /// An added line matched a test marker in a file where a test can live.
    pub test_marker_added: bool,
    /// Files holding at least one counted (not moved) line, first-seen order.
    pub impl_files: Vec<String>,
}

/// Count `added` against `removed` (empty when there are no removals): see the
/// module docs for what is not counted.
pub fn count(
    added: &[AddedLine],
    removed: &[AddedLine],
    is_impl_file: &dyn Fn(&str) -> bool,
    is_test_file: &dyn Fn(&str) -> bool,
    markers: &RegexSet,
) -> Counted {
    let (added_segs, test_marker_added) = impl_segments(added, is_impl_file, is_test_file, markers);
    let removed_segs = impl_segments(removed, is_impl_file, is_test_file, markers).0;
    let moved = match_moved_runs(&added_segs, &removed_segs);

    let mut added_impl_lines = 0usize;
    let mut moved_impl_lines = 0usize;
    let mut impl_files: Vec<String> = Vec::new();
    for (seg, flags) in added_segs.iter().zip(&moved) {
        for (item, is_moved) in seg.iter().zip(flags) {
            if *is_moved {
                moved_impl_lines += 1;
            } else {
                added_impl_lines += 1;
                if !impl_files.iter().any(|f| f == item.file) {
                    impl_files.push(item.file.to_string());
                }
            }
        }
    }
    Counted {
        added_impl_lines,
        moved_impl_lines,
        test_marker_added,
        impl_files,
    }
}

/// The removed lines of a scan; none for a `Failed` / `NotRepo` scan (callers
/// check `Failed` first and fail closed — see `gate::classify_with_removed`).
pub fn removed_lines(removed: &AddedScan) -> &[AddedLine] {
    match removed {
        AddedScan::Lines(r) => r,
        _ => &[],
    }
}

/// Fold the session commits' removed lines into the working-tree ones. A
/// `Failed` side makes the result `Failed` (undetermined, fail closed).
pub fn merge_removed(removed: AddedScan, committed: AddedScan) -> AddedScan {
    match (removed, committed) {
        (AddedScan::Lines(mut r), AddedScan::Lines(c)) => {
            r.extend(c);
            AddedScan::Lines(r)
        }
        (AddedScan::Failed, _) | (_, AddedScan::Failed) => AddedScan::Failed,
        (r, _) => r,
    }
}

/// Narrow removed lines to the files attributed to this session, so another
/// session's deletion cannot vouch for this session's added lines as moved.
/// A `Failed` side stays `Failed` (classify fails closed).
pub fn keep_removed(removed: &AddedScan, keep: &BTreeSet<String>) -> AddedScan {
    match removed {
        AddedScan::Lines(r) => AddedScan::Lines(
            r.iter()
                .filter(|l| keep.contains(&l.file))
                .cloned()
                .collect(),
        ),
        other => other.clone(),
    }
}

/// The sentence of the missing-test block reason that says moves are
/// recognised (so the agent does not reach for `tdd skip` on a pure move).
pub const BLOCK_HINT: &str = "Pure moves are already recognised automatically: a run of 3+ \
     consecutive added lines matching, in order, consecutive lines removed from an \
     implementation file in the same change set (in brace languages ignoring indentation and \
     a leading `pub`/`pub(..)`), and `use`/`mod name;` lines, are not counted above — so what \
     is counted is code that did not move.";

/// The `tdd status` line for moved lines; the empty string (no line) when
/// nothing was moved.
pub fn report_line(moved_impl_lines: usize) -> String {
    if moved_impl_lines == 0 {
        return String::new();
    }
    format!(
        "moved impl lines: {moved_impl_lines} (matched lines removed from impl files; not \
         counted)\n"
    )
}

/// The stderr line saying added implementation lines were read as MOVED code
/// and not counted — printed on every run like `gate::attribution_note`, so
/// an allow earned by a move is never silent. `None` when nothing matched.
pub fn moved_note(v: &Report) -> Option<String> {
    let Determination::Known(Some(f)) = &v.scan else {
        return None;
    };
    (f.moved_impl_lines > 0).then(|| {
        format!(
            "tdd: {} added implementation line(s) matched lines removed from implementation \
             files in the same change set and were read as moved code, not counted \
             ({} counted as new)",
            f.moved_impl_lines, f.added_impl_lines
        )
    })
}

/// Normalise a line for move matching: trim surrounding whitespace, then drop
/// one leading visibility qualifier (`pub `, `pub(crate) `, `pub(super) `,
/// `pub(self) `, `pub(in path) `), so `fn helper` moved and re-exposed as
/// `pub(super) fn helper` still matches.
pub fn normalise_moved_line(text: &str) -> &str {
    let t = text.trim();
    if let Some(rest) = t.strip_prefix("pub(") {
        if let Some(close) = rest.find(')') {
            let after = &rest[close + 1..];
            if after.starts_with(char::is_whitespace) {
                return after.trim_start();
            }
        }
        return t;
    }
    match t.strip_prefix("pub") {
        Some(rest) if rest.starts_with(char::is_whitespace) => rest.trim_start(),
        _ => t,
    }
}

/// `mod name;` (after [`normalise_moved_line`]): a module declaration, not
/// implementation.
fn is_mod_decl(norm: &str) -> bool {
    let Some(rest) = norm.strip_prefix("mod ") else {
        return false;
    };
    let Some(name) = rest.trim().strip_suffix(';') else {
        return false;
    };
    let name = name.trim();
    !name.is_empty()
        && name.chars().all(|c| c.is_alphanumeric() || c == '_')
        && !name.starts_with(|c: char| c.is_ascii_digit())
}

/// (Part of) a `use` tree: comma-separated items, each (braces and the closing
/// `;` aside) empty, a path (`a::b`, `self`, `*`) or `path as name`. Anything
/// else — an `=`, a call, a string, two words without `as` — is not part of a
/// `use`, so a dangling added `use x::{` cannot swallow real code that
/// follows. Residual, stated: a lone path-shaped statement (`break;`, a tail
/// expression `x`) directly after a dangling `use x::{` is still swallowed.
fn is_use_tree_fragment(norm: &str) -> bool {
    let is_path = |w: &str| {
        !w.is_empty()
            && w.chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '_' | ':' | '*'))
    };
    let body = norm.trim_end_matches(';');
    if body.contains(';') {
        return false;
    }
    body.split(',').all(|item| {
        let item: String = item.chars().filter(|c| !matches!(c, '{' | '}')).collect();
        let words: Vec<&str> = item.split_whitespace().collect();
        match words.as_slice() {
            [] => true,
            [p] => is_path(p),
            [p, "as", name] => is_path(p) && is_path(name),
            _ => false,
        }
    })
}

/// The shortest run of consecutive lines recognised as moved (orchestrator
/// ruling: a single line, or a pair, matching some deleted text is too likely
/// to be coincidence — `return true;` deleted with an unrelated fn must not
/// vouch for an edit elsewhere).
pub const MIN_MOVED_RUN: usize = 3;

/// File extensions whose languages delimit blocks with braces, so
/// indentation carries no meaning and is normalised away. Any other impl file
/// (Python, YAML, Haskell, Makefile, no extension, …) keeps its leading
/// whitespace: there a dedent can change control flow.
const BRACE_LANGUAGE_EXTS: &[&str] = &[
    "rs", "go", "c", "h", "cc", "cpp", "hpp", "java", "js", "jsx", "ts", "tsx", "mjs", "cjs", "cs",
    "swift", "kt", "kts", "scala", "php", "dart",
];

fn is_brace_language(file: &str) -> bool {
    Path::new(file)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| BRACE_LANGUAGE_EXTS.contains(&e.to_ascii_lowercase().as_str()))
}

/// The key a line is matched on when looking for moved code: in a brace
/// language, [`normalise_moved_line`] (trim + one visibility qualifier
/// dropped); in any other file, only trailing whitespace is dropped —
/// indentation and every other character are kept.
pub fn move_key<'a>(file: &str, text: &'a str) -> &'a str {
    if is_brace_language(file) {
        normalise_moved_line(text)
    } else {
        text.trim_end()
    }
}

/// One impl line eligible to be part of a moved run.
struct Item<'a> {
    file: &'a str,
    key: &'a str,
}

/// Split lines into segments of impl lines known to be consecutive in their
/// file (same `(file, block)`, see [`AddedLine::block`]). Blank lines and
/// `use ...;` / `mod name;` lines are skipped TRANSPARENTLY: they neither
/// count nor break a segment (they are not implementation, so a moved fn with
/// its imports re-ordered around it is still one run). A test-marker line
/// breaks the segment. Also returns whether a marker matched in a file where
/// a test can live (the gate's only test evidence).
fn impl_segments<'a>(
    lines: &'a [AddedLine],
    is_impl_file: &dyn Fn(&str) -> bool,
    is_test_file: &dyn Fn(&str) -> bool,
    markers: &RegexSet,
) -> (Vec<Vec<Item<'a>>>, bool) {
    let mut segs: Vec<Vec<Item<'a>>> = Vec::new();
    let mut cur: Vec<Item<'a>> = Vec::new();
    let mut cur_at: Option<(&str, u64)> = None;
    let mut test_marker_added = false;
    // Inside a multi-line `use` tree of this file (reset on a file change).
    let mut in_use: Option<&str> = None;
    for AddedLine { file, text, block } in lines {
        let at = (file.as_str(), *block);
        if cur_at != Some(at) {
            if !cur.is_empty() {
                segs.push(std::mem::take(&mut cur));
            }
            cur_at = Some(at);
        }
        if in_use.is_some_and(|f| f != file.as_str()) {
            in_use = None;
        }
        let marker_hit = markers.is_match(text);
        // One arm, one rule: the added line must MATCH A MARKER, and it must
        // land somewhere a test can live (an impl file — an inline `mod tests`
        // — or a test file).
        //
        // Both halves were learned by measurement. The marker requirement came
        // first (backlog 1ff0fcc9): the comment on the impl arm always said
        // "inline test written in an impl file", but the guard that would make
        // it true was missing, so a marker hit in ANY changed path counted —
        // the default set matches the bare substring `test(`, which reaches
        // CHANGELOG.md, a Cargo.toml comment, a JSON fixture or a shell script.
        // The test-file arm was the same hole with the halves swapped: it took
        // ANY added line, marker or not, so a comment, an import or a single
        // blank line under `tests/` was accepted as proof a test was written
        // (backlog 1454ba50 — probed against a real checkout, not reasoned).
        // "A regex matched somewhere" and "a path under tests/ was touched" are
        // both "cannot tell whether a test was added", and resolving that to
        // clean is the fail-open this repo exists to catch. Narrowing can only
        // BLOCK more, never less.
        if marker_hit && (is_impl_file(file) || is_test_file(file)) {
            test_marker_added = true;
            if !cur.is_empty() {
                segs.push(std::mem::take(&mut cur));
            }
            continue;
        }
        if is_test_file(file) {
            continue; // a test path, but this line is not a test
        }
        if is_impl_file(file) && !text.trim().is_empty() {
            let norm = normalise_moved_line(text);
            // `use` statements (single- or multi-line) and `mod name;`
            // declarations are not implementation.
            if in_use.is_some() {
                if is_use_tree_fragment(norm) {
                    if norm.ends_with(';') {
                        in_use = None;
                    }
                    continue;
                }
                in_use = None;
            }
            if let Some(rest) = norm.strip_prefix("use ") {
                if !rest.trim_end().ends_with(';') && is_use_tree_fragment(rest) {
                    in_use = Some(file.as_str());
                    continue;
                }
                if is_use_tree_fragment(rest) {
                    continue;
                }
            }
            if is_mod_decl(norm) {
                continue;
            }
            cur.push(Item {
                file: file.as_str(),
                key: move_key(file, text),
            });
        }
    }
    if !cur.is_empty() {
        segs.push(cur);
    }
    (segs, test_marker_added)
}

/// For each added segment, which of its lines are MOVED: part of a maximal
/// run of at least [`MIN_MOVED_RUN`] consecutive lines whose keys equal, in
/// the same order, consecutive not-yet-consumed lines of ONE removed segment.
/// Greedy, left to right, longest match first; each removed line is consumed
/// at most once (multiset). Lines outside such a run are not moved.
fn match_moved_runs(added: &[Vec<Item<'_>>], removed: &[Vec<Item<'_>>]) -> Vec<Vec<bool>> {
    let mut used: Vec<Vec<bool>> = removed.iter().map(|s| vec![false; s.len()]).collect();
    let mut starts: std::collections::HashMap<&str, Vec<(usize, usize)>> =
        std::collections::HashMap::new();
    for (si, seg) in removed.iter().enumerate() {
        for (j, item) in seg.iter().enumerate() {
            starts.entry(item.key).or_default().push((si, j));
        }
    }
    added
        .iter()
        .map(|seg| {
            let mut moved = vec![false; seg.len()];
            let mut i = 0;
            while i < seg.len() {
                let mut best: Option<(usize, usize, usize)> = None;
                for &(si, j) in starts.get(seg[i].key).map(Vec::as_slice).unwrap_or(&[]) {
                    let r = &removed[si];
                    let mut k = 0;
                    while i + k < seg.len()
                        && j + k < r.len()
                        && !used[si][j + k]
                        && seg[i + k].key == r[j + k].key
                    {
                        k += 1;
                    }
                    if best.is_none_or(|(_, _, b)| k > b) {
                        best = Some((si, j, k));
                    }
                }
                match best {
                    Some((si, j, k)) if k >= MIN_MOVED_RUN => {
                        for d in 0..k {
                            used[si][j + d] = true;
                            moved[i + d] = true;
                        }
                        i += k;
                    }
                    _ => i += 1,
                }
            }
            moved
        })
        .collect()
}

/// Edge cases of moved-code recognition in `classify_with_removed` that the
/// integration tests (`tests/pure_move_not_new_impl.rs`) do not reach.
#[cfg(test)]
mod moved_code {
    use super::*;
    use crate::config::Config;
    use crate::gate::classify_with_removed;
    use crate::git::ChangeScan;

    /// A line in hunk `block`.
    fn lb(file: &str, text: &str, block: u64) -> AddedLine {
        AddedLine {
            file: file.to_string(),
            text: text.to_string(),
            block,
        }
    }

    /// Added lines: one hunk per file.
    fn run(added: &[(&str, &str)], removed: AddedScan) -> Report {
        let v: Vec<AddedLine> = added
            .iter()
            .map(|(f, t)| lb(f, t, 1000 + f.len() as u64))
            .collect();
        run_lines(v, removed)
    }

    fn run_lines(v: Vec<AddedLine>, removed: AddedScan) -> Report {
        let mut files: Vec<String> = v.iter().map(|x| x.file.clone()).collect();
        files.dedup();
        classify_with_removed(
            &Config::default(),
            &ChangeScan::Files(files),
            &AddedScan::Lines(v),
            &removed,
        )
    }

    fn counted(r: &Report) -> (usize, usize) {
        match &r.scan {
            Determination::Known(Some(f)) => (f.added_impl_lines, f.moved_impl_lines),
            _ => panic!("expected a determined scan"),
        }
    }

    #[test]
    fn normalisation_strips_whitespace_and_one_visibility_qualifier() {
        assert_eq!(normalise_moved_line("  fn a() {"), "fn a() {");
        assert_eq!(normalise_moved_line("pub fn a() {"), "fn a() {");
        assert_eq!(normalise_moved_line("pub(crate) fn a() {"), "fn a() {");
        assert_eq!(
            normalise_moved_line("pub(in crate::x) fn a() {"),
            "fn a() {"
        );
        // Not a visibility qualifier: an identifier that starts with `pub`.
        assert_eq!(normalise_moved_line("publish();"), "publish();");
        assert_eq!(normalise_moved_line("pub_x = 1;"), "pub_x = 1;");
    }

    /// A failed removal scan is undetermined, never "no removals" read as clean.
    #[test]
    fn failed_removed_scan_is_undetermined() {
        let r = run(&[("src/a.rs", "fn a() {}")], AddedScan::Failed);
        assert!(matches!(r.scan, Determination::Undetermined(_)));
        assert!(r.blocks(&Config::default()));
    }

    const ABC: [&str; 3] = ["let a = f();", "let b = g(a);", "h(b)"];

    /// The three ABC lines removed from `file` as one hunk.
    fn removed_abc(file: &str, indent: &str) -> AddedScan {
        AddedScan::Lines(
            ABC.iter()
                .map(|t| lb(file, &format!("{indent}{t}"), 1))
                .collect(),
        )
    }

    #[test]
    fn move_key_keeps_indentation_outside_brace_languages() {
        assert_eq!(move_key("a.rs", "    pub fn f() {  "), "fn f() {");
        assert_eq!(move_key("a.TS", "\tlet x = 1;"), "let x = 1;");
        assert_eq!(move_key("a.py", "        return y  "), "        return y");
        assert_eq!(move_key("a.py", "pub x"), "pub x");
        assert_eq!(move_key("Makefile", "\tcc -o x"), "\tcc -o x");
    }

    /// Multiset: one removed run cancels one added copy, not every duplicate.
    #[test]
    fn each_removed_line_cancels_at_most_one_added_line() {
        let mut v: Vec<AddedLine> = ABC.iter().map(|t| lb("src/b.rs", t, 10)).collect();
        v.extend(ABC.iter().map(|t| lb("src/c.rs", t, 11)));
        let r = run_lines(v, removed_abc("src/a.rs", "    "));
        assert_eq!(counted(&r), (3, 3));
    }

    /// A run shorter than MIN_MOVED_RUN never cancels.
    #[test]
    fn two_matching_lines_are_not_a_run() {
        let r = run(
            &[("src/b.rs", ABC[0]), ("src/b.rs", ABC[1])],
            AddedScan::Lines(vec![lb("src/a.rs", ABC[0], 1), lb("src/a.rs", ABC[1], 1)]),
        );
        assert_eq!(counted(&r), (2, 0));
    }

    /// Removed lines from different hunks are not consecutive.
    #[test]
    fn removed_lines_from_separate_hunks_are_not_a_run() {
        let removed = ABC
            .iter()
            .enumerate()
            .map(|(i, t)| lb("src/a.rs", t, 1 + i as u64))
            .collect();
        let added: Vec<(&str, &str)> = ABC.iter().map(|t| ("src/b.rs", *t)).collect();
        let r = run(&added, AddedScan::Lines(removed));
        assert_eq!(counted(&r), (3, 0));
    }

    /// Added lines from different hunks are not consecutive.
    #[test]
    fn added_lines_from_separate_hunks_are_not_a_run() {
        let v = ABC
            .iter()
            .enumerate()
            .map(|(i, t)| lb("src/b.rs", t, 10 + i as u64))
            .collect();
        let r = run_lines(v, removed_abc("src/a.rs", ""));
        assert_eq!(counted(&r), (3, 0));
    }

    /// Same lines in another order are not a move.
    #[test]
    fn reordered_lines_are_not_a_run() {
        let added: Vec<(&str, &str)> = ABC.iter().rev().map(|t| ("src/b.rs", *t)).collect();
        let r = run(&added, removed_abc("src/a.rs", ""));
        assert_eq!(counted(&r), (3, 0));
    }

    /// Blank and `use`/`mod` lines inside a run neither count nor break it.
    #[test]
    fn use_mod_and_blank_lines_are_transparent_inside_a_run() {
        let r = run(
            &[
                ("src/b.rs", ABC[0]),
                ("src/b.rs", "use std::fmt;"),
                ("src/b.rs", ABC[1]),
                ("src/b.rs", ""),
                ("src/b.rs", "mod inner;"),
                ("src/b.rs", ABC[2]),
            ],
            removed_abc("src/a.rs", ""),
        );
        assert_eq!(counted(&r), (0, 3));
    }

    /// Removals from a test file are not implementation that moved.
    #[test]
    fn removal_from_a_test_file_does_not_cancel() {
        let added: Vec<(&str, &str)> = ABC.iter().map(|t| ("src/b.rs", *t)).collect();
        let r = run(&added, removed_abc("tests/t.rs", ""));
        assert_eq!(counted(&r), (3, 0));
    }

    #[test]
    fn use_and_mod_declarations_are_not_impl_lines() {
        let r = run(
            &[
                ("src/b.rs", "use std::fmt;"),
                ("src/b.rs", "pub use crate::a::{b, c as d};"),
                ("src/b.rs", "use std::{"),
                ("src/b.rs", "    collections::HashMap,"),
                ("src/b.rs", "    io::Write as _,"),
                ("src/b.rs", "};"),
                ("src/lib.rs", "mod b;"),
                ("src/lib.rs", "pub(crate) mod c;"),
            ],
            AddedScan::Lines(Vec::new()),
        );
        assert_eq!(counted(&r), (0, 0));
    }

    /// A dangling multi-line `use` does not swallow real code after it, and an
    /// inline `mod x { ... }` is implementation.
    #[test]
    fn dangling_use_does_not_swallow_code_and_inline_mod_counts() {
        let r = run(
            &[
                ("src/b.rs", "use std::{"),
                ("src/b.rs", "let y = compute(1);"),
                ("src/b.rs", "mod inner { fn f() {} }"),
                ("src/b.rs", "use_it(x);"),
            ],
            AddedScan::Lines(Vec::new()),
        );
        assert_eq!(counted(&r), (3, 0));
    }

    /// The parser tags removed lines with the old path and keeps `-- x` / `++ x`
    /// content lines inside a hunk as lines, not headers.
    #[test]
    fn parser_tags_removed_lines_and_reads_hunk_content_as_content() {
        let diff = "\
diff --git a/src/a.rs b/src/a.rs
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,2 +1,2 @@
--- sql comment
-fn gone() {}
+++ kept
diff --git a/src/old.rs b/src/old.rs
--- a/src/old.rs
+++ /dev/null
@@ -1 +0,0 @@
-fn old() {}
";
        let (mut a, mut r) = (Vec::new(), Vec::new());
        crate::git::parse_unified_diff_both(diff, &mut a, &mut r);
        let pairs = |v: &[AddedLine]| -> Vec<(String, String)> {
            v.iter().map(|x| (x.file.clone(), x.text.clone())).collect()
        };
        let p = |f: &str, t: &str| (f.to_string(), t.to_string());
        assert_eq!(pairs(&a), vec![p("src/a.rs", "++ kept")]);
        assert_eq!(
            pairs(&r),
            vec![
                p("src/a.rs", "-- sql comment"),
                p("src/a.rs", "fn gone() {}"),
                p("src/old.rs", "fn old() {}"),
            ]
        );
        // One hunk shares a block id; another hunk gets a different one.
        assert_eq!(r[0].block, r[1].block);
        assert_eq!(a[0].block, r[0].block);
        assert_ne!(r[1].block, r[2].block);
    }
}
