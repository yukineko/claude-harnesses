//! Every blastguard test that runs a child process must bound the wait.
//!
//! `support/bounded.rs::run_bounded` is the one place that waits on a child
//! under a deadline (stdin write, wait and drain, process group killed on
//! timeout). A test file that waits some other way hangs forever if the child
//! hangs, and leaves the child behind as an orphan.
//!
//! This scan walks every `crates/blastguard/tests/**/*.rs` except
//! `support/bounded.rs` and fails, naming `file:line`, for each unbounded
//! child wait. There is no allow-list: a site that is bounded some other way
//! is still reported, so a person decides, not this file.
//!
//! One test fn per construct. The needles are assembled at run time so this
//! file does not contain the constructs it searches for.
//!
//! How a file is read: it is lexed by `proc_macro2` (a real Rust lexer) into
//! tokens with line numbers. Comments are dropped by the lexer and every
//! literal is one opaque token, so text that only names a construct (docs,
//! comments, a script a fixture feeds to another program) is not code. A file
//! the lexer rejects is undetermined and fails the scan; it is never skipped.
//!
//! Scope of the match:
//! - `wait_with_output(`, `.output()` and `.status()` match as token
//!   sequences (identifier followed by `(`, or `.` identifier `(` `)`).
//! - The child-wait construct matches when the receiver is an identifier bound
//!   by a `let` statement that contains a spawn call (the statement may span
//!   lines), or whose name contains `child`. A `Barrier::wait` is neither.
//!   The bound set is per file.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use proc_macro2::{Delimiter, TokenStream, TokenTree};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

const THIS_FILE: &str = "no_unbounded_child_wait.rs";

fn tests_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests")
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let rd = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()));
    for entry in rd {
        let entry = entry.unwrap_or_else(|e| panic!("dir entry in {}: {e}", dir.display()));
        let path = entry.path();
        let ft = entry
            .file_type()
            .unwrap_or_else(|e| panic!("file_type {}: {e}", path.display()));
        if ft.is_dir() {
            collect(&path, out);
        } else if path.extension().is_some_and(|x| x == "rs") {
            out.push(path);
        }
    }
}

/// Every scanned file, relative to `tests/`, with `support/bounded.rs` removed.
fn scanned_files() -> Vec<(String, String)> {
    let root = tests_dir();
    let mut paths = Vec::new();
    collect(&root, &mut paths);
    let mut files = Vec::new();
    let mut saw_helper = false;
    for p in paths {
        let rel = p
            .strip_prefix(&root)
            .expect("under tests/")
            .to_string_lossy()
            .replace('\\', "/");
        if rel == "support/bounded.rs" {
            saw_helper = true;
            continue;
        }
        let src = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {rel}: {e}"));
        files.push((rel, src));
    }
    // An empty or helper-less scan proves nothing: refuse to pass on it.
    assert!(
        saw_helper,
        "support/bounded.rs not found: the scan root is wrong"
    );
    assert!(
        files.iter().any(|(rel, _)| rel == THIS_FILE),
        "{THIS_FILE} not among the scanned files: the scan root is wrong"
    );
    files
}

/// One code token: its text and 1-based source line. A literal is the opaque
/// text `<lit>`; delimiters are their own tokens; comments do not appear.
type Tok = (String, usize);

fn flatten(ts: TokenStream, out: &mut Vec<Tok>) {
    for tt in ts {
        match tt {
            TokenTree::Ident(i) => out.push((i.to_string(), i.span().start().line)),
            TokenTree::Punct(p) => out.push((p.as_char().to_string(), p.span().start().line)),
            TokenTree::Literal(l) => out.push(("<lit>".to_string(), l.span().start().line)),
            TokenTree::Group(g) => {
                let (open, close) = match g.delimiter() {
                    Delimiter::Parenthesis => ("(", ")"),
                    Delimiter::Brace => ("{", "}"),
                    Delimiter::Bracket => ("[", "]"),
                    Delimiter::None => ("", ""),
                };
                if !open.is_empty() {
                    out.push((open.to_string(), g.span_open().start().line));
                }
                flatten(g.stream(), out);
                if !close.is_empty() {
                    out.push((close.to_string(), g.span_close().start().line));
                }
            }
        }
    }
}

/// The code tokens of `src`, or why the lexer rejected it.
fn lex(src: &str) -> Result<Vec<Tok>, String> {
    let ts: TokenStream = src
        .parse()
        .map_err(|e| format!("not lexable as Rust, so undetermined: {e}"))?;
    let mut out = Vec::new();
    flatten(ts, &mut out);
    Ok(out)
}

fn text(toks: &[Tok], i: usize) -> &str {
    toks.get(i).map_or("", |t| t.0.as_str())
}

/// Lines of `name(` (an identifier directly followed by `(`).
fn call_lines(src: &str, name: &str) -> Result<Vec<usize>, String> {
    let toks = lex(src)?;
    Ok((0..toks.len())
        .filter(|&i| text(&toks, i) == name && text(&toks, i + 1) == "(")
        .map(|i| toks[i].1)
        .collect())
}

/// Lines of `.name()` (a method call with no arguments).
fn method_lines(src: &str, name: &str) -> Result<Vec<usize>, String> {
    let toks = lex(src)?;
    Ok((0..toks.len())
        .filter(|&i| {
            text(&toks, i) == "."
                && text(&toks, i + 1) == name
                && text(&toks, i + 2) == "("
                && text(&toks, i + 3) == ")"
        })
        .map(|i| toks[i + 1].1)
        .collect())
}

/// `file:line: text` for every line `find` reports; a file `find` cannot
/// determine is reported too, so it fails the scan.
fn flagged(find: impl Fn(&str) -> Result<Vec<usize>, String>) -> Vec<String> {
    let mut found = Vec::new();
    for (rel, src) in scanned_files() {
        match find(&src) {
            Ok(lines) => {
                let all: Vec<&str> = src.lines().collect();
                for n in lines {
                    found.push(format!("{rel}:{n}: {}", all[n - 1].trim()));
                }
            }
            Err(e) => found.push(format!("{rel}: {e}")),
        }
    }
    found
}

fn assert_none(construct: &str, found: Vec<String>) {
    assert!(
        found.is_empty(),
        "{} unbounded child wait(s) via `{construct}`; use support/bounded.rs run_bounded:\n{}",
        found.len(),
        found.join("\n")
    );
}

#[test]
fn no_wait_with_output_outside_the_bounded_helper() {
    let needle = format!("wait_with{}", "_output");
    assert_none(&format!("{needle}("), flagged(|s| call_lines(s, &needle)));
}

fn wait_needle() -> String {
    format!(".wait{}", "()")
}

fn spawn_needle() -> String {
    format!(".spa{}", "wn(")
}

/// 1-based line numbers of child waits in `src`: `<recv>.wait()` where `recv`
/// was bound by a `let` statement containing a spawn call, or contains `child`.
fn child_wait_lines(src: &str) -> Result<Vec<usize>, String> {
    let toks = lex(src)?;
    let spawn = format!("spa{}", "wn");
    let wait = format!("wa{}", "it");
    let mut bound: HashSet<String> = HashSet::new();
    // Open `let` statements: (brace/paren depth of the `let`, bound identifier).
    let mut open_lets: Vec<(usize, String)> = Vec::new();
    let mut depth = 0usize;
    let mut waits: Vec<(usize, String)> = Vec::new();
    for i in 0..toks.len() {
        match text(&toks, i) {
            "(" | "[" | "{" => depth += 1,
            ")" | "]" | "}" => {
                depth = depth.saturating_sub(1);
                while open_lets.last().is_some_and(|(d, _)| *d > depth) {
                    open_lets.pop();
                }
            }
            ";" => {
                while open_lets.last().is_some_and(|(d, _)| *d >= depth) {
                    open_lets.pop();
                }
            }
            "let" => {
                let mut j = i + 1;
                if text(&toks, j) == "mut" {
                    j += 1;
                }
                open_lets.push((depth, text(&toks, j).to_string()));
            }
            "." if text(&toks, i + 1) == spawn && text(&toks, i + 2) == "(" => {
                if let Some((_, id)) = open_lets.last() {
                    bound.insert(id.clone());
                }
            }
            "." if text(&toks, i + 1) == wait
                && text(&toks, i + 2) == "("
                && text(&toks, i + 3) == ")"
                && i > 0 =>
            {
                waits.push((toks[i + 1].1, text(&toks, i - 1).to_string()));
            }
            _ => {}
        }
    }
    Ok(waits
        .into_iter()
        .filter(|(_, r)| bound.contains(r) || r.contains("child"))
        .map(|(n, _)| n)
        .collect())
}

#[test]
fn no_child_wait_outside_the_bounded_helper() {
    assert_none(
        &format!("child{}", wait_needle()),
        flagged(child_wait_lines),
    );
}

#[test]
fn detector_flags_a_child_bound_to_another_name() {
    let fixture = [
        format!("let mut p = cmd{}).unwrap();", spawn_needle()),
        format!("p{};", wait_needle()),
    ]
    .join("\n");
    assert_eq!(child_wait_lines(&fixture), Ok(vec![2]));
}

#[test]
fn detector_flags_a_renamed_child_spawned_across_lines() {
    let fixture = [
        "let proc = Command::new(bin)".to_string(),
        "    .env(\"A\", \"b\")".to_string(),
        format!("    {})", spawn_needle()),
        "    .unwrap();".to_string(),
        format!("let _ = proc{};", wait_needle()),
    ]
    .join("\n");
    assert_eq!(child_wait_lines(&fixture), Ok(vec![5]));
}

#[test]
fn detector_does_not_flag_barrier_wait() {
    let fixture = [
        "let b = barrier.clone();".to_string(),
        format!("b{};", wait_needle()),
    ]
    .join("\n");
    assert_eq!(child_wait_lines(&fixture), Ok(Vec::<usize>::new()));
}

#[test]
fn detector_does_not_flag_a_wait_on_a_name_bound_after_an_unrelated_let() {
    let fixture = [
        format!("let mut p = cmd{}).unwrap();", spawn_needle()),
        "let q = other.clone();".to_string(),
        format!("q{};", wait_needle()),
    ]
    .join("\n");
    assert_eq!(child_wait_lines(&fixture), Ok(Vec::<usize>::new()));
}

#[test]
fn no_command_output_outside_the_bounded_helper() {
    let name = format!("out{}", "put");
    assert_none(&format!(".{name}()"), flagged(|s| method_lines(s, &name)));
}

#[test]
fn no_command_status_outside_the_bounded_helper() {
    let name = format!("sta{}", "tus");
    assert_none(&format!(".{name}()"), flagged(|s| method_lines(s, &name)));
}

/// Literal and comment forms that must neither hide the code after them nor
/// be read as code themselves.
fn tricky_literals() -> Vec<&'static str> {
    vec![
        r"let p = x.replace('\\', 1);",
        r"let p = x.replace('\'', 1);",
        r#"let p = x.replace('"', 1);"#,
        r#"let p = "\"";"#,
        r##"let p = r#"a " b"#;"##,
        r"let p = b'\\';",
        "fn f<'a>(x: &'a str) {}",
        "/* a /* nested */ still */ let p = 1;",
    ]
}

#[test]
fn literal_forms_do_not_hide_following_code() {
    let name = format!("sta{}", "tus");
    for lit in tricky_literals() {
        let src = format!("{lit}\nlet o = cmd.{name}();");
        assert_eq!(
            method_lines(&src, &name),
            Ok(vec![2]),
            "code after {lit:?} was hidden or mis-read"
        );
    }
}

#[test]
fn text_inside_comments_and_literals_is_not_code() {
    let name = format!("sta{}", "tus");
    let call = format!("x.{name}()");
    let src = [
        format!("// {call}"),
        format!("/* a /* nested {call} */ still comment {call} */"),
        format!("/// doc {call}"),
        format!("let s = \"quoted {call} \\\" more {call}\";"),
        format!("let r = r#\"raw \" {call}\"#;"),
        format!("let c = '\"'; let d = z.{name}();"),
    ]
    .join("\n");
    assert_eq!(method_lines(&src, &name), Ok(vec![6]));
}

#[test]
fn unlexable_source_is_undetermined_not_clean() {
    assert!(lex("let s = \"unterminated;").is_err());
    assert!(call_lines("let s = \"unterminated;", "f").is_err());
    assert!(child_wait_lines("let s = \"unterminated;").is_err());
}
