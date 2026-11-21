//! Bash classification (SPEC §5.1): does a command run a master-only
//! operation (push / rollout / merge into main)?
//!
//! Decided on the brush-parser AST, so strings, comments and heredoc *data*
//! are not mistaken for executed code. The answer is three-valued
//! ([`Determination`]): `Known(None)` = not master-only, `Known(Some(op))` =
//! master-only, `Undetermined` = could not tell, which the gate treats as
//! master-only (the restrictive side).
//!
//! "Could not tell" is raised (never swallowed) when the parser fails, a
//! command name / `-c` / `eval` argument is not a literal, the nesting cap is
//! hit, or the classifier itself panics — in each case only if the text
//! involved carries one of [`SUSPICIOUS`]; text that cannot spell a
//! master-only operation at all has nothing to be undetermined about.

use crate::config;
use crate::group;
use brush_parser::ast;
use brush_parser::word::{WordPiece, WordPieceWithSource};
use harness_core::verdict::Determination;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};

/// Nested-program recursion cap (`-c`, `eval`, substitutions, heredocs).
pub const MAX_DEPTH: usize = 8;
/// Pre-parse caps that keep brush-parser's recursion far from the stack
/// limit (brush aborts — not panics — on stack overflow; blastguard measured
/// the first overflow at nesting 128 on a 2 MiB stack, and the classifier
/// runs on a 64 MiB thread).
const MAX_NESTING: usize = 64;
const MAX_BYTES: usize = 1 << 20;
const STACK_BYTES: usize = 64 << 20;

/// Substrings without which no master-only operation can be spelled.
pub const SUSPICIOUS: [&str; 4] = ["push", "merge", "pull", "rollout-plugins"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Push,
    Rollout,
    Merge,
}

impl Op {
    pub fn kind(self) -> &'static str {
        match self {
            Op::Push => "push",
            Op::Rollout => "rollout",
            Op::Merge => "merge",
        }
    }
}

pub fn suspicious(text: &str) -> bool {
    SUSPICIOUS.iter().any(|s| text.contains(s))
}

/// Classify `command`, run from `cwd`.
pub fn classify(command: &str, cwd: &Path) -> Determination<Option<Op>> {
    let cmd = command.to_string();
    let dir = cwd.to_path_buf();
    let mains = config::main_branches();
    let spawned = std::thread::Builder::new()
        .name("session-master-detect".to_string())
        .stack_size(STACK_BYTES)
        .spawn(move || catch_unwind(AssertUnwindSafe(|| analyze(&cmd, &dir, &mains))));
    let outcome = match spawned {
        Ok(handle) => match handle.join() {
            Ok(Ok(o)) => o,
            Ok(Err(_)) | Err(_) => Outcome::Undetermined("Bash 分類器が panic した".to_string()),
        },
        Err(e) => Outcome::Undetermined(format!("Bash 分類スレッドを起動できない ({e})")),
    };
    match outcome {
        Outcome::Clean => Determination::known(None),
        Outcome::Found(op) => Determination::known(Some(op)),
        Outcome::Undetermined(why) => Determination::undetermined(why),
    }
}

/// Thread-crossing result (`Determination` is minted on the caller thread).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Clean,
    Found(Op),
    Undetermined(String),
}

pub(crate) fn analyze(command: &str, cwd: &Path, mains: &[String]) -> Outcome {
    let mut w = Walker {
        cwd: cwd.to_path_buf(),
        found: None,
        undet: None,
        has_cd: false,
        merges: Vec::new(),
    };
    w.program(command, 0);
    if let Some(op) = w.found {
        return Outcome::Found(op);
    }
    for m in std::mem::take(&mut w.merges) {
        if w.has_cd {
            w.set_undet(
                "merge/pull と同じ program 内に cd/pushd/popd/checkout/switch がある".to_string(),
            );
            continue;
        }
        let dir = match m {
            Ok(d) => d,
            Err(why) => {
                w.set_undet(why);
                continue;
            }
        };
        match group::head_branch(&dir) {
            Ok(Some(b)) if mains.contains(&b) => return Outcome::Found(Op::Merge),
            Ok(_) => {}
            Err(e) => w.set_undet(format!("merge 対象 worktree の HEAD を読めない: {e}")),
        }
    }
    match w.undet {
        Some(why) => Outcome::Undetermined(why),
        None => Outcome::Clean,
    }
}

/// One shell word: its raw text and, when it is a plain literal, its value.
struct Arg {
    raw: String,
    lit: Option<String>,
}

#[derive(Default)]
struct Stdin {
    /// Literal program texts fed on stdin (heredoc bodies / here-strings).
    bodies: Vec<String>,
    /// stdin comes from an earlier pipeline stage (contents unknown).
    piped: bool,
}

struct Walker {
    cwd: PathBuf,
    found: Option<Op>,
    undet: Option<String>,
    has_cd: bool,
    /// Pending `git merge/pull` targets (`Err` = a `-C` that is not literal).
    merges: Vec<Result<PathBuf, String>>,
}

fn opts() -> brush_parser::ParserOptions {
    brush_parser::ParserOptions::default()
}

fn nesting(text: &str) -> usize {
    let mut depth = 0usize;
    let mut max = 0usize;
    for c in text.bytes() {
        match c {
            b'(' | b'{' | b'[' => {
                depth += 1;
                max = max.max(depth);
            }
            b')' | b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    let keywords = text
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| matches!(*w, "if" | "while" | "until" | "for" | "case" | "select"))
        .count();
    max.max(keywords)
}

fn parse_word(raw: &str) -> Option<Vec<WordPieceWithSource>> {
    match catch_unwind(AssertUnwindSafe(|| brush_parser::word::parse(raw, &opts()))) {
        Ok(Ok(p)) => Some(p),
        _ => None,
    }
}

/// The literal value of a word, or `None` when expansion is involved.
fn literal(raw: &str) -> Option<String> {
    let pieces = parse_word(raw)?;
    let mut out = String::new();
    if literal_pieces(&pieces, &mut out, false) {
        Some(out)
    } else {
        None
    }
}

fn literal_pieces(pieces: &[WordPieceWithSource], out: &mut String, quoted: bool) -> bool {
    for p in pieces {
        match &p.piece {
            WordPiece::Text(t) => {
                // Unquoted glob / brace-expansion characters may rewrite the word.
                let expands = !quoted
                    && (t.contains(['*', '?', '['])
                        || (t.contains('{') && (t.contains(',') || t.contains(".."))));
                if expands {
                    return false;
                }
                out.push_str(t);
            }
            WordPiece::SingleQuotedText(t) => out.push_str(t),
            WordPiece::AnsiCQuotedText(t) => {
                if t.contains('\\') {
                    return false;
                }
                out.push_str(t);
            }
            WordPiece::EscapeSequence(s) => {
                let body = s.strip_prefix('\\').unwrap_or(s);
                if body != "\n" {
                    out.push_str(body);
                }
            }
            WordPiece::DoubleQuotedSequence(seq) | WordPiece::GettextDoubleQuotedSequence(seq) => {
                if !literal_pieces(seq, out, true) {
                    return false;
                }
            }
            WordPiece::TildeExpansion(_)
            | WordPiece::ParameterExpansion(_)
            | WordPiece::CommandSubstitution(_)
            | WordPiece::BackquotedCommandSubstitution(_)
            | WordPiece::ArithmeticExpression(_) => return false,
        }
    }
    true
}

fn basename(s: &str) -> &str {
    s.rsplit('/').next().unwrap_or(s)
}

fn unescape_backquote(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\\' {
            if let Some(&n) = it.peek() {
                if matches!(n, '\\' | '`' | '$') {
                    out.push(n);
                    it.next();
                    continue;
                }
            }
        }
        out.push(c);
    }
    out
}

impl Walker {
    fn set_undet(&mut self, why: String) {
        if self.undet.is_none() {
            self.undet = Some(why);
        }
    }

    /// Raise undetermined for `text` when it could spell a master-only op.
    fn suspect(&mut self, text: &str, why: &str) {
        if suspicious(text) {
            self.set_undet(format!(
                "{why}（本文に push/merge/pull/rollout-plugins を含む）"
            ));
        }
    }

    fn found(&mut self, op: Op) {
        if self.found.is_none() {
            self.found = Some(op);
        }
    }

    fn program(&mut self, src: &str, depth: usize) {
        if depth > MAX_DEPTH {
            self.suspect(src, "再帰深さ上限を超えた");
            return;
        }
        if src.len() > MAX_BYTES || nesting(src) > MAX_NESTING {
            self.suspect(src, "構文が大きすぎる/深すぎるので解析しない");
            return;
        }
        let parsed = catch_unwind(AssertUnwindSafe(|| {
            brush_parser::Parser::new(src.as_bytes(), &opts()).parse_program()
        }));
        let prog = match parsed {
            Ok(Ok(p)) => p,
            Ok(Err(_)) | Err(_) => {
                self.suspect(src, "構文解析に失敗した");
                return;
            }
        };
        for cc in &prog.complete_commands {
            self.list(cc, src, depth);
        }
    }

    fn list(&mut self, l: &ast::CompoundList, src: &str, depth: usize) {
        for item in &l.0 {
            let aol = &item.0;
            self.pipeline(&aol.first, src, depth);
            for a in &aol.additional {
                match a {
                    ast::AndOr::And(p) | ast::AndOr::Or(p) => self.pipeline(p, src, depth),
                }
            }
        }
    }

    fn pipeline(&mut self, p: &ast::Pipeline, src: &str, depth: usize) {
        for (i, c) in p.seq.iter().enumerate() {
            self.command(c, src, depth, i > 0);
        }
    }

    fn command(&mut self, c: &ast::Command, src: &str, depth: usize, piped: bool) {
        match c {
            ast::Command::Simple(sc) => self.simple(sc, src, depth, piped),
            ast::Command::Compound(cc, redirs) => {
                self.compound(cc, src, depth);
                self.redirect_list(redirs.as_ref(), depth);
            }
            ast::Command::Function(fd) => {
                self.compound(&fd.body.0, src, depth);
                self.redirect_list(fd.body.1.as_ref(), depth);
            }
            ast::Command::ExtendedTest(e, redirs) => {
                self.ext_test(&e.expr, depth);
                self.redirect_list(redirs.as_ref(), depth);
            }
        }
    }

    fn ext_test(&mut self, e: &ast::ExtendedTestExpr, depth: usize) {
        match e {
            ast::ExtendedTestExpr::And(a, b) | ast::ExtendedTestExpr::Or(a, b) => {
                self.ext_test(a, depth);
                self.ext_test(b, depth);
            }
            ast::ExtendedTestExpr::Not(a) | ast::ExtendedTestExpr::Parenthesized(a) => {
                self.ext_test(a, depth)
            }
            ast::ExtendedTestExpr::UnaryTest(_, w) => self.scan_word(&w.value, depth),
            ast::ExtendedTestExpr::BinaryTest(_, a, b) => {
                self.scan_word(&a.value, depth);
                self.scan_word(&b.value, depth);
            }
        }
    }

    fn compound(&mut self, cc: &ast::CompoundCommand, src: &str, depth: usize) {
        use ast::CompoundCommand as C;
        match cc {
            C::Arithmetic(a) => self.scan_word(&a.expr.value, depth),
            C::ArithmeticForClause(f) => {
                for e in [&f.initializer, &f.condition, &f.updater]
                    .into_iter()
                    .flatten()
                {
                    self.scan_word(&e.value, depth);
                }
                self.list(&f.body.list, src, depth);
            }
            C::BraceGroup(b) => self.list(&b.list, src, depth),
            C::Subshell(s) => self.list(&s.list, src, depth),
            C::ForClause(f) => {
                for w in f.values.iter().flatten() {
                    self.scan_word(&w.value, depth);
                }
                self.list(&f.body.list, src, depth);
            }
            C::CaseClause(c) => {
                self.scan_word(&c.value.value, depth);
                for item in &c.cases {
                    for p in &item.patterns {
                        self.scan_word(&p.value, depth);
                    }
                    if let Some(l) = &item.cmd {
                        self.list(l, src, depth);
                    }
                }
            }
            C::IfClause(i) => {
                self.list(&i.condition, src, depth);
                self.list(&i.then, src, depth);
                for e in i.elses.iter().flatten() {
                    if let Some(c) = &e.condition {
                        self.list(c, src, depth);
                    }
                    self.list(&e.body, src, depth);
                }
            }
            C::WhileClause(w) | C::UntilClause(w) => {
                self.list(&w.0, src, depth);
                self.list(&w.1.list, src, depth);
            }
            C::Coprocess(c) => self.command(&c.body, src, depth, false),
        }
    }

    fn redirect_list(&mut self, r: Option<&ast::RedirectList>, depth: usize) {
        let mut sink = Stdin::default();
        for io in r.iter().flat_map(|l| l.0.iter()) {
            self.redirect(io, depth, &mut sink);
        }
    }

    fn redirect(&mut self, io: &ast::IoRedirect, depth: usize, stdin: &mut Stdin) {
        match io {
            ast::IoRedirect::File(_, _, target) => match target {
                ast::IoFileRedirectTarget::Filename(w)
                | ast::IoFileRedirectTarget::Duplicate(w) => self.scan_word(&w.value, depth),
                ast::IoFileRedirectTarget::ProcessSubstitution(_, sub) => {
                    self.list(&sub.list, "", depth)
                }
                ast::IoFileRedirectTarget::Fd(_) => {}
            },
            ast::IoRedirect::HereDocument(_, hd) => {
                if hd.requires_expansion {
                    self.scan_heredoc(&hd.doc.value, depth);
                }
                stdin.bodies.push(hd.doc.value.clone());
            }
            ast::IoRedirect::HereString(_, w) => {
                self.scan_word(&w.value, depth);
                match literal(&w.value) {
                    Some(s) => stdin.bodies.push(s),
                    None => stdin.bodies.push(w.value.clone()),
                }
            }
            ast::IoRedirect::OutputAndError(w, _) => self.scan_word(&w.value, depth),
        }
    }

    /// Walk command substitutions hidden inside a word.
    fn scan_word(&mut self, raw: &str, depth: usize) {
        if !raw.contains("$(") && !raw.contains('`') && !raw.contains("<(") && !raw.contains(">(") {
            return;
        }
        match parse_word(raw) {
            Some(pieces) => self.walk_pieces(&pieces, raw, depth),
            None => self.suspect(raw, "word を解析できない"),
        }
    }

    fn scan_heredoc(&mut self, body: &str, depth: usize) {
        if !body.contains("$(") && !body.contains('`') {
            return;
        }
        let parsed = catch_unwind(AssertUnwindSafe(|| {
            brush_parser::word::parse_heredoc(body, &opts())
        }));
        match parsed {
            Ok(Ok(pieces)) => self.walk_pieces(&pieces, body, depth),
            _ => self.suspect(body, "heredoc 本文を解析できない"),
        }
    }

    fn walk_pieces(&mut self, pieces: &[WordPieceWithSource], raw: &str, depth: usize) {
        for p in pieces {
            match &p.piece {
                WordPiece::CommandSubstitution(s) => self.program(s, depth + 1),
                WordPiece::BackquotedCommandSubstitution(s) => {
                    self.program(&unescape_backquote(s), depth + 1)
                }
                WordPiece::DoubleQuotedSequence(seq)
                | WordPiece::GettextDoubleQuotedSequence(seq) => self.walk_pieces(seq, raw, depth),
                WordPiece::ParameterExpansion(_) => {
                    let slice = raw.get(p.start_index..p.end_index).unwrap_or(raw);
                    if slice.contains("$(") || slice.contains('`') {
                        self.suspect(slice, "パラメータ展開の中のコマンド置換は解析しない");
                    }
                }
                WordPiece::ArithmeticExpression(e) => {
                    if e.value != raw {
                        self.scan_word(&e.value, depth);
                    }
                }
                WordPiece::Text(_)
                | WordPiece::SingleQuotedText(_)
                | WordPiece::AnsiCQuotedText(_)
                | WordPiece::EscapeSequence(_)
                | WordPiece::TildeExpansion(_) => {}
            }
        }
    }

    fn simple(&mut self, sc: &ast::SimpleCommand, src: &str, depth: usize, piped: bool) {
        let mut words: Vec<&ast::Word> = Vec::new();
        let mut stdin = Stdin {
            bodies: Vec::new(),
            piped,
        };
        let items = sc
            .prefix
            .iter()
            .flat_map(|p| p.0.iter())
            .map(|i| (i, true))
            .chain(
                sc.suffix
                    .iter()
                    .flat_map(|s| s.0.iter())
                    .map(|i| (i, false)),
            );
        let mut name_pushed = false;
        for (item, is_prefix) in items {
            if !is_prefix && !name_pushed {
                if let Some(w) = &sc.word_or_name {
                    words.push(w);
                }
                name_pushed = true;
            }
            match item {
                ast::CommandPrefixOrSuffixItem::IoRedirect(r) => {
                    self.redirect(r, depth, &mut stdin)
                }
                ast::CommandPrefixOrSuffixItem::Word(w) => words.push(w),
                ast::CommandPrefixOrSuffixItem::AssignmentWord(_, w) => {
                    if is_prefix {
                        self.scan_word(&w.value, depth);
                    } else {
                        words.push(w);
                    }
                }
                ast::CommandPrefixOrSuffixItem::ProcessSubstitution(_, sub) => {
                    self.list(&sub.list, src, depth)
                }
            }
        }
        if !name_pushed {
            if let Some(w) = &sc.word_or_name {
                words.push(w);
            }
        }
        for w in &words {
            self.scan_word(&w.value, depth);
        }
        let argv: Vec<Arg> = words
            .iter()
            .map(|w| Arg {
                raw: w.value.clone(),
                lit: literal(&w.value),
            })
            .collect();
        self.exec(&argv, &stdin, src, depth);
    }

    fn suspect_args(&mut self, argv: &[Arg], why: &str) {
        let joined: Vec<&str> = argv.iter().map(|a| a.raw.as_str()).collect();
        self.suspect(&joined.join(" "), why);
    }

    /// Strip wrappers, then judge the command that actually runs.
    fn exec(&mut self, argv: &[Arg], stdin: &Stdin, src: &str, depth: usize) {
        let mut i = 0usize;
        let mut via_xargs = false;
        loop {
            let Some(a) = argv.get(i) else {
                return;
            };
            let Some(name) = a.lit.as_deref() else {
                self.suspect_args(&argv[i..], "command 名が literal でない");
                return;
            };
            let next = match basename(name) {
                "env" => match self.skip_env(argv, i + 1, depth) {
                    Some(n) => n,
                    None => return,
                },
                "command" => {
                    let mut j = i + 1;
                    while let Some(o) = argv.get(j).and_then(|x| x.lit.as_deref()) {
                        match o {
                            "-v" | "-V" => return,
                            "-p" => j += 1,
                            "--" => {
                                j += 1;
                                break;
                            }
                            _ => break,
                        }
                    }
                    j
                }
                "exec" => skip_opts(argv, i + 1, &["-a"]),
                "builtin" | "nohup" => i + 1,
                "time" => skip_opts(argv, i + 1, &["-f", "-o"]),
                "nice" => {
                    let mut j = i + 1;
                    while let Some(o) = argv.get(j).and_then(|x| x.lit.as_deref()) {
                        if o == "-n" || o == "--adjustment" {
                            j += 2;
                        } else if o.starts_with('-') && o.len() > 1 {
                            j += 1;
                        } else {
                            break;
                        }
                    }
                    j
                }
                "timeout" => {
                    let j = skip_opts(argv, i + 1, &["-s", "-k", "--signal", "--kill-after"]);
                    j + 1
                }
                "sudo" => skip_opts(
                    argv,
                    i + 1,
                    &[
                        "-u", "-g", "-h", "-p", "-C", "-D", "-r", "-t", "-U", "-T", "-R",
                    ],
                ),
                "xargs" => {
                    via_xargs = true;
                    skip_opts(
                        argv,
                        i + 1,
                        &[
                            "-I", "-L", "-n", "-P", "-s", "-d", "-E", "-a", "-J", "-R", "-S",
                        ],
                    )
                }
                _ => break,
            };
            if argv[i + 1..next.min(argv.len())]
                .iter()
                .any(|x| x.lit.is_none())
            {
                self.suspect_args(&argv[i..], "wrapper の option が literal でない");
            }
            i = next;
        }
        let rest = &argv[i..];
        let Some(name) = rest.first().and_then(|a| a.lit.as_deref()) else {
            return;
        };
        let args = &rest[1..];
        match basename(name) {
            "git" => self.git(args, via_xargs, src),
            "cd" | "pushd" | "popd" => self.has_cd = true,
            "rollout-plugins.sh" => self.found(Op::Rollout),
            "sh" | "bash" | "zsh" | "dash" => self.shell(args, stdin, src, depth),
            "source" | "." => {
                if let Some(a) = args.first() {
                    match a.lit.as_deref() {
                        Some(s) if basename(s) == "rollout-plugins.sh" => self.found(Op::Rollout),
                        Some(_) => {}
                        None => self.suspect(&a.raw, "source の引数が literal でない"),
                    }
                }
            }
            "eval" => {
                let mut parts = Vec::new();
                for a in args {
                    match &a.lit {
                        Some(s) => parts.push(s.clone()),
                        None => {
                            self.suspect_args(rest, "eval の引数が literal でない");
                            return;
                        }
                    }
                }
                self.program(&parts.join(" "), depth + 1);
            }
            "condukt" if positionals(args, &[]).starts_with(&["worktree", "merge"]) => {
                self.found(Op::Merge)
            }
            "gh" if positionals(args, &["-R", "--repo"]).starts_with(&["pr", "merge"]) => {
                self.found(Op::Merge)
            }
            // A literal program that is none of the above cannot itself be
            // push / rollout / merge, whatever its (possibly non-literal) args.
            _ => {}
        }
    }

    /// `env [opts] [NAME=VAL...] cmd`. `-S STRING` is a nested program.
    fn skip_env(&mut self, argv: &[Arg], mut j: usize, depth: usize) -> Option<usize> {
        while let Some(a) = argv.get(j) {
            let Some(s) = a.lit.as_deref() else {
                return Some(j);
            };
            if s == "--" {
                return Some(j + 1);
            }
            if s == "-S" || s == "--split-string" {
                if let Some(n) = argv.get(j + 1) {
                    match &n.lit {
                        Some(body) => {
                            let mut text = body.clone();
                            for extra in &argv[j + 2..] {
                                text.push(' ');
                                text.push_str(&extra.raw);
                            }
                            self.program(&text, depth + 1);
                        }
                        None => self.suspect_args(&argv[j..], "env -S の引数が literal でない"),
                    }
                }
                return None;
            }
            if let Some(body) = s
                .strip_prefix("--split-string=")
                .or_else(|| s.strip_prefix("-S"))
            {
                let mut text = body.to_string();
                for extra in &argv[j + 1..] {
                    text.push(' ');
                    text.push_str(&extra.raw);
                }
                self.program(&text, depth + 1);
                return None;
            }
            if matches!(s, "-u" | "-C" | "--unset" | "--chdir" | "-P") {
                j += 2;
            } else if s.starts_with('-') || (s.contains('=') && !s.starts_with('=')) {
                j += 1;
            } else {
                return Some(j);
            }
        }
        Some(j)
    }

    fn git(&mut self, args: &[Arg], via_xargs: bool, src: &str) {
        let mut j = 0usize;
        let mut dir: Result<PathBuf, String> = Ok(self.cwd.clone());
        while let Some(a) = args.get(j) {
            let Some(s) = a.lit.as_deref() else {
                self.suspect_args(args, "git の subcommand / option が literal でない");
                return;
            };
            match s {
                "-C" => {
                    match args.get(j + 1).map(|x| x.lit.clone()) {
                        Some(Some(p)) => {
                            if let Ok(d) = &dir {
                                dir = Ok(d.join(p));
                            }
                        }
                        Some(None) => dir = Err("git -C の引数が literal でない".to_string()),
                        None => {}
                    }
                    j += 2;
                }
                "-c" => {
                    if let Some(v) = args.get(j + 1) {
                        if v.raw.contains("alias.") || v.lit.is_none() {
                            self.suspect(&v.raw, "git -c の alias / 非 literal 設定");
                        }
                    }
                    j += 2;
                }
                "--git-dir" | "--work-tree" | "--namespace" | "--config-env" | "--super-prefix"
                | "--list-cmds" => j += 2,
                _ if s.starts_with('-') => j += 1,
                _ => break,
            }
        }
        let Some(sub) = args.get(j).and_then(|a| a.lit.as_deref()) else {
            if via_xargs {
                self.suspect(src, "xargs から git の subcommand が渡る");
            }
            return;
        };
        match sub {
            "push" => self.found(Op::Push),
            "merge" | "pull" => self.merges.push(dir),
            "checkout" | "switch" => self.has_cd = true,
            _ => {}
        }
    }

    fn shell(&mut self, args: &[Arg], stdin: &Stdin, src: &str, depth: usize) {
        let mut j = 0usize;
        let mut has_c = false;
        let mut reads_stdin = false;
        while let Some(a) = args.get(j) {
            let Some(s) = a.lit.as_deref() else {
                break;
            };
            if s == "--" || s == "-" {
                j += 1;
                break;
            }
            if matches!(s, "-o" | "+o" | "-O" | "+O" | "--rcfile" | "--init-file") {
                j += 2;
                continue;
            }
            if s.starts_with("--") {
                j += 1;
                continue;
            }
            if (s.starts_with('-') || s.starts_with('+')) && s.len() > 1 {
                if s.starts_with('-') && s[1..].contains('c') {
                    has_c = true;
                }
                if s.starts_with('-') && s[1..].contains('s') {
                    reads_stdin = true;
                }
                j += 1;
                continue;
            }
            break;
        }
        if has_c {
            if let Some(a) = args.get(j) {
                match &a.lit {
                    Some(body) => self.program(body, depth + 1),
                    None => self.suspect_args(args, "-c の引数が literal でない"),
                }
            }
            return;
        }
        if !reads_stdin {
            if let Some(a) = args.get(j) {
                match a.lit.as_deref() {
                    Some(s) if basename(s) == "rollout-plugins.sh" => self.found(Op::Rollout),
                    Some(_) => {}
                    None => self.suspect(&a.raw, "shell の script 引数が literal でない"),
                }
                return;
            }
        }
        if stdin.bodies.is_empty() {
            if stdin.piped {
                self.suspect(src, "pipeline から shell に渡る stdin の中身は読めない");
            }
            return;
        }
        for b in &stdin.bodies {
            self.program(b, depth + 1);
        }
    }
}

/// Skip `-x` options (those in `with_arg` consume the next word) and a
/// trailing `--`; return the index of the first non-option word.
fn skip_opts(argv: &[Arg], mut j: usize, with_arg: &[&str]) -> usize {
    while let Some(s) = argv.get(j).and_then(|a| a.lit.as_deref()) {
        if s == "--" {
            return j + 1;
        }
        if !s.starts_with('-') || s.len() < 2 {
            break;
        }
        if with_arg.contains(&s) {
            j += 2;
        } else {
            j += 1;
        }
    }
    j
}

/// Literal positional words (options skipped; `with_arg` consume a value).
fn positionals<'a>(args: &'a [Arg], with_arg: &[&str]) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut j = 0usize;
    while let Some(a) = args.get(j) {
        match a.lit.as_deref() {
            Some(s) if with_arg.contains(&s) => j += 2,
            Some(s) if s.starts_with('-') => j += 1,
            Some(s) => {
                out.push(s);
                j += 1;
            }
            None => {
                out.push("\u{0}");
                j += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(cmd: &str) -> Outcome {
        analyze(
            cmd,
            Path::new("/nonexistent-dir-for-test"),
            &["main".to_string()],
        )
    }

    #[test]
    fn plain_and_hidden_push() {
        for c in [
            "git push",
            "FOO=1 git -C . push",
            "bash -c 'git push'",
            "echo $(git push)",
            "echo `git push`",
            "env A=1 nice -n 5 git push",
            "sudo -u x git push",
            "timeout 5 git push",
            "f() { git push; }",
            "if true; then git push; fi",
            "bash <<EOF\ngit push\nEOF",
            "eval git push",
            "cat <(git push)",
            "env -S 'git push'",
        ] {
            assert_eq!(run(c), Outcome::Found(Op::Push), "{c}");
        }
    }

    #[test]
    fn data_is_not_code() {
        for c in [
            "echo git push",
            "git commit -m \"git push する\"",
            "cat <<'EOF'\ngit push\nEOF",
            "git log --grep push",
            "git status",
            "# git push",
        ] {
            assert_eq!(run(c), Outcome::Clean, "{c}");
        }
    }

    #[test]
    fn rollout_forms() {
        for c in [
            "scripts/rollout-plugins.sh",
            "bash scripts/rollout-plugins.sh",
            ". scripts/rollout-plugins.sh",
        ] {
            assert_eq!(run(c), Outcome::Found(Op::Rollout), "{c}");
        }
    }

    #[test]
    fn nonliteral_name_with_push_is_undetermined() {
        assert!(matches!(run("$G push"), Outcome::Undetermined(_)));
        assert!(matches!(
            run("bash -c \"$X\" push"),
            Outcome::Undetermined(_)
        ));
        assert!(matches!(run("echo push | bash"), Outcome::Undetermined(_)));
        assert_eq!(run("$G status"), Outcome::Clean);
    }

    #[test]
    fn merge_with_unreadable_head_is_undetermined() {
        assert!(matches!(run("git merge x"), Outcome::Undetermined(_)));
        assert!(matches!(
            run("cd /x && git merge y"),
            Outcome::Undetermined(_)
        ));
        assert_eq!(run("gh pr merge 3"), Outcome::Found(Op::Merge));
        assert_eq!(
            run("condukt worktree merge --run r"),
            Outcome::Found(Op::Merge)
        );
    }

    #[test]
    fn depth_cap() {
        let mut c = "git push".to_string();
        for _ in 0..12 {
            c = format!("eval {}", shell_quote(&c));
        }
        assert!(matches!(run(&c), Outcome::Undetermined(_)));
    }

    fn shell_quote(s: &str) -> String {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}
