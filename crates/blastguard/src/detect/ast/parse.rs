//! Parse stage of `detect::ast`: pre-scan (`MAX_PARSE_BYTES` /
//! `MAX_PARSE_DEPTH` / zsh-only syntax / unterminated heredoc), then
//! brush-parser 0.4.0 under `catch_unwind`.
//!
//! Every failure is an `Err(Cause)` that `analyze` turns into
//! `Analysis::Undetermined`; this stage never produces a program for an input
//! it could not fully parse.
//!
//! # Why a structural pre-scan, and why these caps (measured 2026-11-16)
//!
//! brush-parser's PEG grammar and its word parser recurse once per nesting
//! level, and running out of stack is an **abort** (not a panic), so
//! `catch_unwind` cannot contain it. The depth has to be bounded *before*
//! brush is called.
//!
//! Measured with a throwaway bench (scratchpad, not committed) calling
//! `brush_parser::Parser::new(..).parse_program()` (and `word::parse` for the
//! `word:` rows) on a thread with a 2 MiB stack — the default spawned-thread
//! stack, the one `tests.rs::analyze_bounded` uses — on darwin, brush-parser
//! 0.4.0. Times are one run each; "SO" = stack overflow (process abort).
//!
//! | nesting kind (fixture)            | depth 32 release | depth 32 debug | first SO, debug | first SO, release |
//! |-----------------------------------|------------------|----------------|-----------------|-------------------|
//! | `{ ` .. `echo hi;` .. ` }`        | 0.03 ms          | 0.18 ms        | 128             | 512               |
//! | `{ ` .. ` }` unbalanced (error)   | 0.13 ms          | 0.29 ms        | 128             | 512               |
//! | `(` .. `echo hi` .. `)`           | 0.02 ms          | 0.42 ms        | none to 512     | none to 512       |
//! | `if true; then ` .. `fi; `        | 0.22 ms          | 1.07 ms        | 128             | 512               |
//! | `while true; do ` .. `done; `     | 0.24 ms          | 0.82 ms        | 128             | 512               |
//! | `for x in a; do ` .. `done; `     | 0.16 ms          | 0.90 ms        | 128             | 512               |
//! | `case a in a) ` .. ` ;; esac`     | 0.21 ms          | 0.43 ms        | 128             | 512               |
//! | `$(` .. `)`                       | 0.02 ms          | 0.08 ms        | 256             | none to 512       |
//! | `"$(` .. `)"`                     | 0.02 ms          | —              | —               | none to 512       |
//! | mixed (all of the above in turn)  | 0.05 ms          | 0.27 ms        | none to 512     | none to 512       |
//! | `[[ ! ! .. a ]]` (`!` chain)      | —                | —              | 256             | —                 |
//! | word: `${a:-` .. `}`              | —                | —              | 128             | —                 |
//! | word: `"$(` .. `)"`               | —                | —              | 512             | —                 |
//!
//! Time grows roughly linearly with depth in every kind measured (e.g. brace
//! groups, release: 0.06 ms at 64, 0.2 ms at 128, 1.5 ms at 256); the
//! "200-deep brace groups take ~48 s" super-linearity reported earlier was
//! NOT reproduced by these fixtures, so the binding constraint observed here
//! is the stack, not time. Size, release: 64 KiB of flat `echo hi; ` parses
//! in ~9 ms, ~70 KiB of depth-32 brace groups side by side in ~11 ms (debug:
//! ~0.08–0.56 s and ~0.08–0.45 s across two runs).
//!
//! Chosen caps:
//!
//! * `MAX_PARSE_DEPTH = 16` (user ruling 2026-11-16, basis: block-log max
//!   13, below): the shallowest stack overflow observed is at depth 128 in a
//!   debug build on a 2 MiB stack (`{ }`, `if`, `while`, `for`, `case`,
//!   `${a:-}`; 96 passes), so 16 leaves an 8x margin in the worst
//!   build/stack; parse time at depth 32 (twice the cap) is ≤ 1.1 ms debug,
//!   ≤ 0.25 ms release.
//! * `MAX_PARSE_BYTES = 8 KiB` (user ruling 2026-11-16; 3 of 417 commands in
//!   the block log exceed it): inputs of ~8.8–9.2 KiB parse in ~1–3 ms
//!   release (flat `echo hi; ` 9216 B: 1.1 / 2.7 / 1.2 ms over three runs;
//!   depth-32 brace groups side by side, 8832 B: 3.0 / 1.2 ms), ~10–21 ms
//!   debug. Even 64 KiB stayed ≤ ~11 ms release.
//! * No wall-clock cap (`Cause::Timeout` is not produced here): with both
//!   caps the measured worst case is about one order of magnitude under
//!   100 ms in release (64 KiB: ~9-11 ms; at the actual 8 KiB cap: ~1-3 ms),
//!   and a watchdog thread could not stop a runaway brush call anyway
//!   (it can only stop waiting for it).
//!
//! # The depth pre-scan is conservative (it over-counts)
//!
//! [`prescan`] rejects the input as `TooDeep` when either of two counts
//! exceeds `MAX_PARSE_DEPTH`:
//!
//! 1. [`bracket_depth`], the plain counter of the user ruling (2026-11-16):
//!    +1 on `(` `{` `[`, -1 on `)` `}` `]` (floor 0), quoting ignored, so a
//!    bracket inside quotes or a heredoc counts too. Its distribution over
//!    the 417-row block log (measured by the main session, not here): max 13
//!    (0:158 1:60 2:67 3:52 4:31 5:15 6:11 7:9 8:5 9:1 10:3 11:2 12:3 13:1),
//!    so the cap of 16 refuses no observed command.
//! 2. A quote-aware structural scan ([`Scan`]) that additionally counts
//!    compound-command keywords and `!` chains, which the bracket counter
//!    does not see. It counts every `(` (`$(`, `<(`, `$((`, arrays,
//!    extglob), every `{` (`${`, brace groups, brace expansion), every
//!    backtick substitution, and the keywords `if` / `while` / `until` /
//!    `for` / `case` / `select`, plus the number of consecutive `!` words in
//!    one command. A closer (`)`, `}`, `fi` / `done` / `esac`, a closing
//!    backtick) only removes the innermost frame when it is of the matching
//!    kind; a mismatched closer is ignored, so an input the scanner misreads
//!    only ever looks *deeper*, never shallower. This scan is a
//!    restrictive-only pre-filter, accepted as such by user ruling 2026-11-16.
//!    Text is skipped only where
//!    bash cannot execute it: single-quoted strings, `$'..'`, comments, and
//!    the body of a heredoc whose delimiter is quoted (only once its
//!    terminator line is found). Unquoted heredoc bodies are scanned for
//!    `$(`, `${` and backticks.

use super::{Cause, MAX_PARSE_BYTES, MAX_PARSE_DEPTH};
use std::panic::{catch_unwind, AssertUnwindSafe};

/// Parse `command` into a brush-parser program, or say why it cannot be.
pub(super) fn parse(command: &str) -> Result<brush_parser::ast::Program, Cause> {
    if command.len() > MAX_PARSE_BYTES {
        return Err(Cause::TooLong);
    }
    prescan(command)?;
    let options = parser_options();
    let parsed = catch_unwind(AssertUnwindSafe(|| {
        brush_parser::Parser::new(command.as_bytes(), &options).parse_program()
    }));
    match parsed {
        Ok(Ok(program)) => Ok(program),
        Ok(Err(e)) => Err(Cause::Parse(e.to_string())),
        Err(_) => Err(Cause::Panic),
    }
}

/// Parse one word (as found in the program returned by [`parse`]) into its
/// pieces, with the same options and the same panic containment.
pub(super) fn parse_word(
    word: &str,
) -> Result<Vec<brush_parser::word::WordPieceWithSource>, Cause> {
    let options = parser_options();
    match catch_unwind(AssertUnwindSafe(|| {
        brush_parser::word::parse(word, &options)
    })) {
        Ok(Ok(pieces)) => Ok(pieces),
        Ok(Err(e)) => Err(Cause::Parse(e.to_string())),
        Err(_) => Err(Cause::Panic),
    }
}

/// bash mode (not POSIX / sh), extglob on — brush's defaults.
pub(super) fn parser_options() -> brush_parser::ParserOptions {
    brush_parser::ParserOptions::default()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Frame {
    /// `(`; `arith` when it opens `((` / `$((`.
    Paren {
        arith: bool,
    },
    /// `{`; `in_dq` when opened as `${` inside double quotes (single quotes
    /// are literal there).
    Brace {
        in_dq: bool,
    },
    /// `if` / `while` / `until` / `for` / `case` / `select`.
    Keyword,
    Backtick,
    /// Inside `"..."`: not counted as depth.
    DoubleQuote,
    /// Inside an unquoted heredoc body: like double quotes, but `"` is
    /// literal and the frame ends with the body. Not counted as depth.
    HereBody,
}

impl Frame {
    fn counts(self) -> bool {
        !matches!(self, Frame::DoubleQuote | Frame::HereBody)
    }
}

struct Pending {
    delim: Vec<u8>,
    strip_tabs: bool,
    quoted: bool,
}

/// Bytes that end a word for the purposes of the pre-scan.
fn is_delim(c: u8) -> bool {
    matches!(
        c,
        b' ' | b'\t' | b'\n' | b';' | b'&' | b'|' | b'(' | b')' | b'<' | b'>'
    )
}

/// Pre-scan `command` before brush sees it: nesting depth (over-counted,
/// see the module doc), zsh-only syntax, unterminated heredocs.
pub(super) fn prescan(command: &str) -> Result<(), Cause> {
    if bracket_depth(command.as_bytes()) > MAX_PARSE_DEPTH {
        return Err(Cause::TooDeep);
    }
    Scan::new(command.as_bytes(), Vec::new(), 0).run()
}

/// The user-specified plain counter (ruling 2026-11-16): +1 on `(` `{` `[`,
/// -1 on `)` `}` `]` (floor 0), quoting ignored; the maximum reached.
fn bracket_depth(b: &[u8]) -> usize {
    let mut depth = 0usize;
    let mut max = 0usize;
    for &c in b {
        match c {
            b'(' | b'{' | b'[' => {
                depth += 1;
                max = max.max(depth);
            }
            b')' | b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    max
}

struct Scan<'a> {
    b: &'a [u8],
    i: usize,
    stack: Vec<Frame>,
    /// Counted frames inherited from an enclosing scan (heredoc body).
    base_depth: usize,
    /// Consecutive `!` words in the current command.
    bang: usize,
    pending: Vec<Pending>,
}

impl<'a> Scan<'a> {
    fn new(b: &'a [u8], stack: Vec<Frame>, base_depth: usize) -> Self {
        Scan {
            b,
            i: 0,
            stack,
            base_depth,
            bang: 0,
            pending: Vec::new(),
        }
    }

    fn at(&self, i: usize) -> Option<u8> {
        self.b.get(i).copied()
    }

    fn top(&self) -> Option<Frame> {
        self.stack.last().copied()
    }

    fn in_arith(&self) -> bool {
        self.stack
            .iter()
            .any(|f| matches!(f, Frame::Paren { arith: true }))
    }

    fn word_start(&self) -> bool {
        self.i == 0 || self.at(self.i - 1).is_some_and(is_delim)
    }

    fn check_depth(&self) -> Result<(), Cause> {
        let depth = self.base_depth + self.stack.iter().filter(|f| f.counts()).count() + self.bang;
        if depth > MAX_PARSE_DEPTH {
            Err(Cause::TooDeep)
        } else {
            Ok(())
        }
    }

    fn push(&mut self, f: Frame) -> Result<(), Cause> {
        self.stack.push(f);
        self.check_depth()
    }

    fn pop_if(&mut self, pred: impl Fn(Frame) -> bool) {
        if self.top().is_some_and(pred) {
            self.stack.pop();
        }
    }

    fn run(mut self) -> Result<(), Cause> {
        while self.i < self.b.len() {
            let quoted_ctx = matches!(self.top(), Some(Frame::DoubleQuote | Frame::HereBody));
            if quoted_ctx {
                self.step_quoted()?;
            } else if !self.step_normal()? {
                // An unterminated single / ANSI-C quote: brush reports it.
                return Ok(());
            }
        }
        if self.pending.is_empty() {
            Ok(())
        } else {
            Err(Cause::Parse("unterminated here-document".to_string()))
        }
    }

    /// One step inside `"..."` or an unquoted heredoc body.
    fn step_quoted(&mut self) -> Result<(), Cause> {
        let c = self.b[self.i];
        let next = self.at(self.i + 1);
        match c {
            b'\\' => self.i += 2,
            b'"' if self.top() == Some(Frame::DoubleQuote) => {
                self.stack.pop();
                self.i += 1;
            }
            b'`' => {
                self.push(Frame::Backtick)?;
                self.i += 1;
            }
            b'$' if next == Some(b'(') => {
                let arith = self.at(self.i + 2) == Some(b'(');
                self.push(Frame::Paren { arith })?;
                self.i += 2;
            }
            b'$' if next == Some(b'{') => {
                if self.at(self.i + 2) == Some(b'(') {
                    return Err(Cause::ZshOnly("${(flags)...}".to_string()));
                }
                self.push(Frame::Brace { in_dq: true })?;
                self.i += 2;
            }
            _ => self.i += 1,
        }
        Ok(())
    }

    /// One step in unquoted shell text. `Ok(false)`: stop scanning (an
    /// unterminated quote — brush rejects the input).
    fn step_normal(&mut self) -> Result<bool, Cause> {
        let c = self.b[self.i];
        let next = self.at(self.i + 1);
        let literal_squote = self.top() == Some(Frame::Brace { in_dq: true });
        match c {
            b'\\' => self.i += 2,
            b'\'' if !literal_squote => {
                match self.b[self.i + 1..].iter().position(|&x| x == b'\'') {
                    Some(p) => self.i += p + 2,
                    None => return Ok(false),
                }
            }
            b'$' if next == Some(b'\'') && !literal_squote => {
                let mut j = self.i + 2;
                loop {
                    match self.at(j) {
                        None => return Ok(false),
                        Some(b'\\') => j += 2,
                        Some(b'\'') => break,
                        Some(_) => j += 1,
                    }
                }
                self.i = j + 1;
            }
            b'"' => {
                self.stack.push(Frame::DoubleQuote);
                self.i += 1;
            }
            b'`' => {
                if self.top() == Some(Frame::Backtick) {
                    self.stack.pop();
                } else {
                    self.push(Frame::Backtick)?;
                }
                self.i += 1;
            }
            b'#' if self.word_start() && !self.in_arith() => {
                // A comment runs to the newline (which is not consumed).
                self.i = self.b[self.i..]
                    .iter()
                    .position(|&x| x == b'\n')
                    .map_or(self.b.len(), |p| self.i + p);
            }
            b'(' => {
                if self.i >= 1 && self.at(self.i - 1) == Some(b'=') {
                    let before = self.i.checked_sub(2).and_then(|k| self.at(k));
                    if before.is_none_or(is_delim) {
                        return Err(Cause::ZshOnly("=(cmd)".to_string()));
                    }
                }
                self.push(Frame::Paren {
                    arith: next == Some(b'('),
                })?;
                self.i += 1;
            }
            b')' => {
                self.pop_if(|f| matches!(f, Frame::Paren { .. }));
                self.i += 1;
            }
            b'{' => {
                if self.i >= 1 && self.at(self.i - 1) == Some(b'$') && next == Some(b'(') {
                    return Err(Cause::ZshOnly("${(flags)...}".to_string()));
                }
                self.push(Frame::Brace { in_dq: false })?;
                self.i += 1;
            }
            b'}' => {
                self.pop_if(|f| matches!(f, Frame::Brace { .. }));
                self.i += 1;
            }
            b'<' if next == Some(b'<') => {
                if self.at(self.i + 2) == Some(b'<') {
                    self.i += 3;
                } else if self.in_arith() {
                    self.i += 2;
                } else {
                    self.heredoc_operator();
                }
            }
            b'\n' => {
                self.bang = 0;
                self.i += 1;
                self.heredoc_bodies()?;
            }
            b';' | b'&' | b'|' => {
                self.bang = 0;
                self.i += 1;
            }
            b'!' if self.word_start() && self.at(self.i + 1).is_none_or(is_delim) => {
                self.bang += 1;
                self.check_depth()?;
                self.i += 1;
            }
            b'a'..=b'z' if self.word_start() => {
                let len = self.b[self.i..]
                    .iter()
                    .take_while(|x| x.is_ascii_lowercase())
                    .count();
                let end = self.i + len;
                if self.at(end).is_none_or(is_delim) {
                    match &self.b[self.i..end] {
                        b"if" | b"while" | b"until" | b"for" | b"case" | b"select" => {
                            self.push(Frame::Keyword)?;
                        }
                        b"fi" | b"done" | b"esac" => self.pop_if(|f| f == Frame::Keyword),
                        _ => {}
                    }
                }
                self.i = end;
            }
            _ => self.i += 1,
        }
        Ok(true)
    }

    /// At `<<` (not `<<<`): read `-` and the delimiter word, queue the body.
    fn heredoc_operator(&mut self) {
        let mut j = self.i + 2;
        let strip_tabs = self.at(j) == Some(b'-');
        if strip_tabs {
            j += 1;
        }
        while matches!(self.at(j), Some(b' ' | b'\t')) {
            j += 1;
        }
        let mut delim = Vec::new();
        let mut quoted = false;
        while let Some(c) = self.at(j) {
            match c {
                b'\'' | b'"' => {
                    quoted = true;
                    let close = self.b[j + 1..].iter().position(|&x| x == c);
                    match close {
                        Some(p) => {
                            delim.extend_from_slice(&self.b[j + 1..j + 1 + p]);
                            j += p + 2;
                        }
                        // Unterminated quote in the delimiter: let the main
                        // scan see the quote (and brush reject it).
                        None => break,
                    }
                }
                b'\\' => {
                    quoted = true;
                    if let Some(x) = self.at(j + 1) {
                        delim.push(x);
                    }
                    j += 2;
                }
                c if is_delim(c) => break,
                c => {
                    delim.push(c);
                    j += 1;
                }
            }
        }
        if !delim.is_empty() {
            self.pending.push(Pending {
                delim,
                strip_tabs,
                quoted,
            });
        }
        self.i = j;
    }

    /// After a newline: consume the queued heredoc bodies, in order.
    fn heredoc_bodies(&mut self) -> Result<(), Cause> {
        for p in std::mem::take(&mut self.pending) {
            let start = self.i;
            let mut line = start;
            let end = loop {
                if line >= self.b.len() {
                    return Err(Cause::Parse("unterminated here-document".to_string()));
                }
                let nl = self.b[line..]
                    .iter()
                    .position(|&x| x == b'\n')
                    .map_or(self.b.len(), |k| line + k);
                let mut text = &self.b[line..nl];
                if p.strip_tabs {
                    while let Some((b'\t', rest)) = text.split_first() {
                        text = rest;
                    }
                }
                if text == p.delim.as_slice() {
                    break (line, (nl + 1).min(self.b.len()));
                }
                line = nl + 1;
            };
            let (body_end, after) = end;
            if !p.quoted {
                let stack = vec![Frame::HereBody];
                let depth = self.base_depth + self.stack.iter().filter(|f| f.counts()).count();
                Scan::new(&self.b[start..body_end], stack, depth).run()?;
            }
            self.i = after;
        }
        Ok(())
    }
}
