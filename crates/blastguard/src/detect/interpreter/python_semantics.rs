//! What a Python program does beyond its literal OS-command calls, for the
//! Python-file judge ([`super::python_file_body_verdict`]): the names it binds
//! to command runners (aliases), the paths it removes or overwrites, the
//! modules whose code it loads, and the SQL it runs. Kept apart from
//! `python_file.rs` for the file-size cap. The path-mutating part of the
//! filesystem reading ([`path_mutation_verdict`]) also judges inline Python
//! programs (`python -c`, a program read from stdin; backlog f6dbff5b).
//!
//! Every reading here is fail-closed: what cannot be resolved statically (a
//! runner name passed around, a computed path not proven repo-scoped by
//! [`values`], a module whose code is not in the text, computed SQL) is an
//! Ask, never an Allow.

use std::collections::{HashMap, HashSet};

use super::super::{analyze_shell_payload, Ctx, Decision};

mod method_refs;
mod values;
pub(super) use values::{argv_word, Argv};

/// One lexical item, read with Python's rules. `Name` is a dotted name
/// (`os.path.join`) with its byte span in the source; `Nl` ends a logical
/// statement (a newline outside brackets, or `;`).
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Tok {
    Name {
        text: String,
        start: usize,
        end: usize,
    },
    Str {
        text: String,
        fstring: bool,
    },
    Punct(char),
    Nl,
}

/// Lex `src`. `#` comments are dropped, a backslash-newline joins lines, and
/// a string prefix (`r`, `b`, `f`, `rb`, …) belongs to its string. A
/// non-raw string's text is its value with escapes decoded
/// ([`super::python_escape::decode_non_raw`]); a raw string's text, and a
/// non-raw body that cannot be decoded statically (`\N{…}`), is its body with
/// every backslash pair `\X` read as `X` (not Python's value for a raw string:
/// Python keeps the backslash). An
/// unterminated string runs to the end of the text (the caller has already
/// refused a body that does not tokenise).
///
/// A body with a `\N{…}` or a malformed escape keeps that undecoded text HERE
/// only; its true reading ([`super::python_escape::Decoded::Holed`] /
/// `Undecodable`) is what [`literal_values`] reports, and the interpreter
/// write check judges a holed or undecodable write target from that.
pub(super) fn lex(src: &str) -> Vec<Tok> {
    lex_impl(src, None)
}

/// The decoded value of every NON-raw string literal of `src`, read with the
/// same lexer as [`lex`] (comments, prefixes and triple quotes handled), in
/// source order. Raw literals are not listed: their text is their body.
pub(super) fn literal_values(src: &str) -> Vec<super::python_escape::Decoded> {
    let mut values = Vec::new();
    lex_impl(src, Some(&mut values));
    values
}

fn lex_impl(src: &str, mut values: Option<&mut Vec<super::python_escape::Decoded>>) -> Vec<Tok> {
    let chars: Vec<(usize, char)> = src.char_indices().collect();
    let byte_at = |i: usize| chars.get(i).map_or(src.len(), |(b, _)| *b);
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i].1;
        if c == '#' {
            while i < chars.len() && chars[i].1 != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '\\' && chars.get(i + 1).map(|p| p.1) == Some('\n') {
            i += 2;
            continue;
        }
        if c == '\n' || c == ';' {
            if (depth == 0 || c == ';') && out.last() != Some(&Tok::Nl) {
                out.push(Tok::Nl);
            }
            i += 1;
            continue;
        }
        if c == '\'' || c == '"' {
            let (mut fstring, mut raw, mut bytes) = (false, false, false);
            if let Some(Tok::Name { text, end, .. }) = out.last() {
                if *end == byte_at(i)
                    && text.len() <= 2
                    && text.chars().all(|p| "rRbBuUfF".contains(p))
                {
                    fstring = text.contains(['f', 'F']);
                    raw = text.contains(['r', 'R']);
                    bytes = text.contains(['b', 'B']);
                    out.pop();
                }
            }
            let triple = chars.get(i + 1).map(|p| p.1) == Some(c)
                && chars.get(i + 2).map(|p| p.1) == Some(c);
            let q = if triple { 3 } else { 1 };
            i += q;
            let body_start = byte_at(i);
            let mut body_end = src.len();
            let mut s = String::new();
            while i < chars.len() {
                let d = chars[i].1;
                if d == '\\' {
                    if let Some(&(_, e)) = chars.get(i + 1) {
                        s.push(e);
                    }
                    i += 2;
                    continue;
                }
                if d == c
                    && (!triple
                        || (chars.get(i + 1).map(|p| p.1) == Some(c)
                            && chars.get(i + 2).map(|p| p.1) == Some(c)))
                {
                    body_end = byte_at(i);
                    i += q;
                    break;
                }
                s.push(d);
                i += 1;
            }
            // A non-raw literal's value is its body with escapes decoded the
            // way Python does — notably a backslash-newline is REMOVED, so
            // `'/etc/ho\<LF>sts'` is `/etc/hosts` (backlog 47b20647). A raw
            // literal, or a body `decode_non_raw` cannot decode statically
            // (`\N{…}`, a malformed escape), keeps the reading above here; the
            // undecoded reading is reported through `values`.
            if !raw {
                let body = src.get(body_start..body_end.max(body_start)).unwrap_or("");
                let decoded = super::python_escape::decode_non_raw(body, bytes);
                if let super::python_escape::Decoded::Value(v) = &decoded {
                    s = v.clone();
                }
                if let Some(values) = values.as_deref_mut() {
                    values.push(decoded);
                }
            }
            out.push(Tok::Str { text: s, fstring });
            continue;
        }
        if c.is_alphanumeric() || c == '_' {
            let start = i;
            while i < chars.len() {
                let d = chars[i].1;
                let dotted = d == '.'
                    && chars
                        .get(i + 1)
                        .is_some_and(|p| p.1.is_alphanumeric() || p.1 == '_');
                if d.is_alphanumeric() || d == '_' || dotted {
                    i += 1;
                } else {
                    break;
                }
            }
            let (s, e) = (byte_at(start), byte_at(i));
            out.push(Tok::Name {
                text: src[s..e].to_string(),
                start: s,
                end: e,
            });
            continue;
        }
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
        if !c.is_whitespace() {
            out.push(Tok::Punct(c));
        }
        i += 1;
    }
    out
}

fn name(t: Option<&Tok>) -> Option<&str> {
    match t {
        Some(Tok::Name { text, .. }) => Some(text),
        _ => None,
    }
}

fn statements(toks: &[Tok]) -> Vec<&[Tok]> {
    toks.split(|t| *t == Tok::Nl)
        .filter(|s| !s.is_empty())
        .collect()
}

fn top(module: &str) -> &str {
    module.split('.').next().unwrap_or(module)
}

/// Modules whose names, once bound to another name, are followed by the
/// rewrite: the ones that run commands, remove files, open files or run SQL.
const WATCHED_MODULES: &[&str] = &[
    "os",
    "posix",
    "posixpath",
    "subprocess",
    "commands",
    "shutil",
    "pathlib",
    "pty",
    "asyncio",
    "io",
    "codecs",
    "builtins",
    "sqlite3",
];

/// Callables that run a command or remove a file. A reference to one that is
/// not a direct call (`f(os.system)`, `run = subprocess.run` then `g(run)`)
/// escapes the reading and is an Ask. Matched on the canonical (rewritten)
/// name, and on any attribute of it.
const RUNNERS: &[&str] = &[
    "os.system",
    "os.popen",
    "os.execv",
    "os.execve",
    "os.execl",
    "os.execle",
    "os.execlp",
    "os.execlpe",
    "os.execvp",
    "os.execvpe",
    "os.spawnl",
    "os.spawnle",
    "os.spawnlp",
    "os.spawnlpe",
    "os.spawnv",
    "os.spawnve",
    "os.spawnvp",
    "os.spawnvpe",
    "os.posix_spawn",
    "os.posix_spawnp",
    "os.remove",
    "os.unlink",
    "os.rmdir",
    "os.removedirs",
    "subprocess.run",
    "subprocess.call",
    "subprocess.check_call",
    "subprocess.check_output",
    "subprocess.Popen",
    "subprocess.getoutput",
    "subprocess.getstatusoutput",
    "commands.getoutput",
    "commands.getstatusoutput",
    "shutil.rmtree",
    "pty.spawn",
    "asyncio.create_subprocess_shell",
    "asyncio.create_subprocess_exec",
];

/// Modules that are themselves a way to run commands or remove files: a bare
/// reference to one as a value (`g(os)`) escapes the reading too.
const RUNNER_MODULES: &[&str] = &["os", "posix", "subprocess", "commands", "shutil", "pty"];

/// Process spawners the shared call judge does not know, renamed to the call
/// it judges the same way: `pty.spawn(argv)` and
/// `asyncio.create_subprocess_exec(prog, *args)` run an argv like
/// `subprocess.run([...])`; `asyncio.create_subprocess_shell(line)` runs a
/// shell line like `os.system(line)`.
const SPAWNER_AS: &[(&str, &str)] = &[
    ("pty.spawn", "subprocess.run"),
    ("asyncio.create_subprocess_exec", "subprocess.run"),
    ("asyncio.create_subprocess_shell", "os.system"),
];

/// The names a program binds, read from its code view.
#[derive(Default)]
pub(super) struct Bindings {
    /// alias → canonical dotted name (`sp` → `subprocess`, `s` → `os.system`).
    aliases: HashMap<String, String>,
    /// Top-level modules imported (absolute imports only).
    pub(super) imported: Vec<String>,
    /// The first relative import (`from . import x`), if any.
    pub(super) relative: bool,
    /// A `from <runner module> import *`, if any.
    pub(super) star_runner: Option<String>,
}

impl Bindings {
    fn resolve(&self, n: &str) -> String {
        let (first, rest) = match n.split_once('.') {
            Some((f, r)) => (f, Some(r)),
            None => (n, None),
        };
        match (self.aliases.get(first), rest) {
            (Some(a), Some(r)) => format!("{a}.{r}"),
            (Some(a), None) => a.clone(),
            (None, _) => n.to_string(),
        }
    }

    fn bind(&mut self, alias: &str, target: String) {
        if !alias.contains('.') && WATCHED_MODULES.contains(&top(&target)) && alias != target {
            self.aliases.insert(alias.to_string(), target);
        }
    }
}

/// Read `import X as Y`, `from X import f [as g]` (parenthesised lists too)
/// and simple `name = dotted.name` assignments, in order, from `code`.
pub(super) fn bindings(code: &str) -> Bindings {
    let toks = lex(code);
    let mut b = Bindings::default();
    for st in statements(&toks) {
        match name(st.first()) {
            Some("import") => {
                for item in st[1..].split(|t| *t == Tok::Punct(',')) {
                    let Some(module) = name(item.first()) else {
                        continue;
                    };
                    b.imported.push(top(module).to_string());
                    if let (Some("as"), Some(alias)) = (name(item.get(1)), name(item.get(2))) {
                        let target = b.resolve(module);
                        b.bind(alias, target);
                    }
                }
            }
            Some("from") => {
                if st.get(1) == Some(&Tok::Punct('.')) {
                    b.relative = true;
                    continue;
                }
                let Some(module) = name(st.get(1)) else {
                    continue;
                };
                if name(st.get(2)) != Some("import") {
                    continue;
                }
                b.imported.push(top(module).to_string());
                let items: Vec<Tok> = st[3..]
                    .iter()
                    .filter(|t| !matches!(t, Tok::Punct('(' | ')')))
                    .cloned()
                    .collect();
                for item in items.split(|t| *t == Tok::Punct(',')) {
                    if item.first() == Some(&Tok::Punct('*')) {
                        if RUNNER_MODULES.contains(&top(module)) || top(module) == "asyncio" {
                            b.star_runner.get_or_insert(module.to_string());
                        }
                        continue;
                    }
                    let Some(n) = name(item.first()) else {
                        continue;
                    };
                    let alias = match (name(item.get(1)), name(item.get(2))) {
                        (Some("as"), Some(a)) => a,
                        _ => n,
                    };
                    let target = format!("{}.{n}", b.resolve(module));
                    b.bind(alias, target);
                }
            }
            Some(lhs) if st.len() == 3 && st[1] == Tok::Punct('=') => {
                if let Some(rhs) = name(st.get(2)) {
                    let target = b.resolve(rhs);
                    b.bind(lhs, target);
                }
            }
            _ => {}
        }
    }
    b
}

/// `src` with every name that starts with an alias rewritten to its canonical
/// spelling, and every spawner in [`SPAWNER_AS`] renamed. Strings and
/// comments are untouched; an attribute (`x.s`, `f().s`) is not a name.
pub(super) fn rewrite(src: &str, b: &Bindings) -> String {
    let toks = lex(src);
    let mut out = src.to_string();
    for (k, t) in toks.iter().enumerate().rev() {
        let Tok::Name { text, start, end } = t else {
            continue;
        };
        if k > 0 && toks[k - 1] == Tok::Punct('.') {
            continue;
        }
        let mut canonical = b.resolve(text);
        if let Some((_, to)) = SPAWNER_AS.iter().find(|(from, _)| *from == canonical) {
            canonical = (*to).to_string();
        }
        if canonical != *text {
            out.replace_range(*start..*end, &canonical);
        }
    }
    out
}

/// Last dotted components that start a process or hand a line to a shell,
/// whatever object they are reached through (`os.execv`, `posix.execv`,
/// `ctypes.CDLL(None).execl`, `pty.spawn`, …). Used by [`references_runner`].
const PROCESS_STARTING_ATTRS: &[&str] = &[
    "system",
    "popen",
    "getoutput",
    "getstatusoutput",
    "spawn",
    "fork",
    "forkpty",
    "startfile",
    "create_subprocess_shell",
    "create_subprocess_exec",
    "Popen",
    "execv",
    "execve",
    "execl",
    "execle",
    "execlp",
    "execlpe",
    "execvp",
    "execvpe",
    "spawnl",
    "spawnle",
    "spawnlp",
    "spawnlpe",
    "spawnv",
    "spawnve",
    "spawnvp",
    "spawnvpe",
    "posix_spawn",
    "posix_spawnp",
];

/// The first name in `code` (a code view with aliases rewritten, see
/// [`rewrite`]) through which a string of the program could reach a process
/// or a shell — called or not: a [`RUNNERS`] callable (or an attribute of
/// one), a bare [`RUNNER_MODULES`] module used as a value, any attribute of
/// `subprocess` / `pty` / `commands` / `asyncio`, or any name whose last
/// dotted component is in [`PROCESS_STARTING_ATTRS`] (an attribute reached
/// through a call result, `ctypes.CDLL(None).execl`, included). `None` when
/// no token is one of those. Read with the Python lexer ([`lex`]), so a name
/// spelled inside a string literal or a comment is not a reference.
pub(super) fn references_runner(code: &str) -> Option<String> {
    for t in lex(code) {
        let Tok::Name { text: n, .. } = t else {
            continue;
        };
        let last = n.rsplit('.').next().unwrap_or(&n);
        let runner = RUNNERS
            .iter()
            .any(|r| n == *r || n.strip_prefix(r).is_some_and(|x| x.starts_with('.')));
        if runner
            || RUNNER_MODULES.contains(&n.as_str())
            || ["subprocess", "pty", "commands", "asyncio"].contains(&top(&n))
            || PROCESS_STARTING_ATTRS.contains(&last)
        {
            return Some(n);
        }
    }
    None
}

/// The first command-running callable or runner module that `code`
/// (rewritten) references without calling it directly, outside the
/// statements that bind it (`import …`, `from … import …`, and the
/// `x = os.system` alias, which rewrites to `os.system = os.system`).
pub(super) fn escaped_runner(code: &str) -> Option<String> {
    let toks = lex(code);
    for st in statements(&toks) {
        if matches!(name(st.first()), Some("import" | "from")) {
            continue;
        }
        if st.len() == 3 && st[1] == Tok::Punct('=') && name(st.first()) == name(st.get(2)) {
            continue;
        }
        for (k, t) in st.iter().enumerate() {
            let Some(n) = name(Some(t)) else { continue };
            if k > 0 && st[k - 1] == Tok::Punct('.') {
                continue;
            }
            let runner = RUNNERS
                .iter()
                .any(|r| n == *r || n.strip_prefix(r).is_some_and(|x| x.starts_with('.')));
            let called = st.get(k + 1) == Some(&Tok::Punct('('));
            if (runner && !called) || RUNNER_MODULES.contains(&n) {
                return Some(n.to_string());
            }
        }
    }
    None
}

/// A path a program names, as far as it can be read statically: a literal
/// (Python never expands `~` in one), the user's home plus a suffix, or — for
/// a path computed by iterating a directory ([`values`]) — "somewhere at or
/// below a repo-relative directory".
#[derive(Debug, Clone, PartialEq)]
enum PathVal {
    Lit(String),
    Home(String),
    /// Some path at or below the repo-relative directory `root` (a
    /// [`values::dir_root`] spelling: relative, no `~` start, no `..` and no
    /// dot-named component), reached by iterating it (`glob.glob`,
    /// `Path.rglob`, `os.walk`, `os.listdir` joined back onto its directory).
    /// `hidden` when the iteration may yield dot-named entries below `root`.
    Under {
        root: String,
        hidden: bool,
    },
    /// One entry name of a directory listing (`os.listdir`, the name lists of
    /// `os.walk`): one component, never `.` / `..`, never holding `/`, maybe
    /// dot-named. As a path on its own it is an entry of the current
    /// directory (`Under { root: ".", hidden: true }`).
    Entry,
    /// A name list of an `os.walk` step: iterating it yields [`PathVal::Entry`].
    /// Not a path.
    Entries,
}

impl PathVal {
    /// `os.path.join` / `/` of two values, `None` when the result is not one
    /// of the readable shapes (a `..` below an iterated root, a listing
    /// joined onto the home directory, a name list used as a path).
    fn join(self, other: PathVal) -> Option<PathVal> {
        use PathVal::{Entries, Entry, Home, Lit, Under};
        let cwd_entry = || Under {
            root: ".".to_string(),
            hidden: true,
        };
        Some(match (self, other) {
            (Entries, _) | (_, Entries) => return None,
            (_, h @ Home(_)) => h,
            (_, Lit(b)) if b.starts_with('/') => Lit(b),
            (Lit(a), Lit(b)) if a.is_empty() || a == "." => Lit(b),
            (Lit(a), Lit(b)) => Lit(format!("{}/{b}", a.trim_end_matches('/'))),
            (Home(a), Lit(b)) => Home(format!("{}/{b}", a.trim_end_matches('/'))),
            (Home(_), _) => return None,
            (Lit(a), Entry) => Under {
                root: values::dir_root(&a)?,
                hidden: true,
            },
            (Lit(a), u @ Under { .. }) if a.is_empty() || a == "." => u,
            (Lit(_), Under { .. }) | (Under { .. }, Under { .. }) => return None,
            (Under { root, .. }, Entry) => Under { root, hidden: true },
            (Under { root, hidden }, Lit(b)) => {
                // Evaluated first: `hidden || below(..)?` would skip the `..`
                // check whenever `hidden` is already true.
                let dotted = values::below(&b)?;
                Under {
                    hidden: hidden || dotted,
                    root,
                }
            }
            (Entry, b) => return cwd_entry().join(b),
        })
    }

    fn concat(self, other: PathVal) -> Option<PathVal> {
        match (self, other) {
            (PathVal::Lit(a), PathVal::Lit(b)) => Some(PathVal::Lit(a + &b)),
            (PathVal::Home(a), PathVal::Lit(b)) => Some(PathVal::Home(a + &b)),
            _ => None,
        }
    }

    fn expanduser(self) -> Option<PathVal> {
        match self {
            PathVal::Lit(s) if s == "~" => Some(PathVal::Home(String::new())),
            PathVal::Lit(s) if s.starts_with("~/") => Some(PathVal::Home(s[1..].to_string())),
            PathVal::Lit(s) if s.starts_with('~') => None,
            // An entry may be named `~` or `~user`.
            PathVal::Entry | PathVal::Entries => None,
            other => Some(other),
        }
    }

    /// The path as one shell word: `~` left bare so the shell rules see the
    /// home directory, everything else quoted when it is not plain. `None`
    /// for the iterated shapes, which are judged by [`values::under_verdict`].
    fn shell_word(&self) -> Option<String> {
        Some(match self {
            PathVal::Lit(s) => shell_quote(s),
            PathVal::Home(rest) => match rest.strip_prefix('/') {
                Some(r) if !r.is_empty() => format!("~/{}", shell_quote(r)),
                _ => "~".to_string(),
            },
            PathVal::Under { .. } | PathVal::Entry | PathVal::Entries => return None,
        })
    }
}

fn shell_quote(w: &str) -> String {
    if !w.is_empty()
        && w.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_/.,:=@%+-".contains(c))
    {
        w.to_string()
    } else {
        format!("'{}'", w.replace('\'', "'\\''"))
    }
}

/// Every value an expression may have; more than [`MAX_VALUES`] is treated as
/// unreadable.
type Vals = Vec<PathVal>;
const MAX_VALUES: usize = 32;

fn cross(a: &Vals, b: &Vals, f: impl Fn(PathVal, PathVal) -> Option<PathVal>) -> Option<Vals> {
    if a.len() * b.len() > MAX_VALUES {
        return None;
    }
    let mut out = Vec::new();
    for x in a {
        for y in b {
            out.push(f(x.clone(), y.clone())?);
        }
    }
    Some(out)
}

/// One `for` target binding: (target index, target count, iterable span).
type LoopBind = (usize, usize, (usize, usize));

/// The static evaluator over one program's tokens.
///
/// A plain name has a value only when every binding of it in the program is a
/// simple `name = <expr>` statement or a simple `for` target over a readable
/// iteration ([`values`]); its values are then those of every such binding,
/// wherever it sits (so a path reassigned between two `open(p, 'w')` calls is
/// judged on both, whatever the control flow). A name bound any other way —
/// `as p`, a `def` / `lambda` parameter, a tuple or chained target, `p += …`,
/// `p := …`, `global` / `del` / `import`, a comprehension target, a subscript
/// or attribute target — is in `rebound` and has no value. Two further
/// readings live in [`values`]: `sys.argv[N]` (from the command that runs the
/// file) and `self.attr` (from the constructor calls of its class).
struct Eval<'a> {
    toks: &'a [Tok],
    src: &'a str,
    assigns: HashMap<String, Vec<(usize, usize)>>,
    /// `for` targets: name → (target index, target count, iterable span).
    loops: HashMap<String, Vec<LoopBind>>,
    rebound: HashSet<String>,
    /// Plain names bound some way other than as a `def` parameter, a simple
    /// assignment or a simple `for` target (a subset of `rebound`).
    nonparam: HashSet<String>,
    /// Names bound by an `import` / `from … import` statement.
    imported: HashSet<String>,
    /// Statement spans `[pos, end)` in token order.
    stmts: Vec<(usize, usize)>,
    /// The arguments of every run of this file, as [`values::Argv`].
    argv: Option<&'a values::Argv>,
    /// Remaining name lookups (bounds cyclic or deep definitions).
    budget: std::cell::Cell<usize>,
}

/// True when the token at `k` of `st` is an assignment `=` at bracket depth
/// 0, not part of `==`, `<=`, `+=`, … (`depth` is the depth at `k`).
fn is_assign_eq(st: &[Tok], k: usize, depth: usize) -> bool {
    depth == 0
        && st[k] == Tok::Punct('=')
        && st.get(k + 1) != Some(&Tok::Punct('='))
        && !matches!(
            k.checked_sub(1).map(|p| &st[p]),
            Some(Tok::Punct(c)) if "=<>!+-*/%&|^@:".contains(*c)
        )
}

impl<'a> Eval<'a> {
    fn new(toks: &'a [Tok], src: &'a str, argv: Option<&'a values::Argv>) -> Eval<'a> {
        let mut e = Eval {
            toks,
            src,
            assigns: HashMap::new(),
            loops: HashMap::new(),
            rebound: HashSet::new(),
            nonparam: HashSet::new(),
            imported: HashSet::new(),
            stmts: Vec::new(),
            argv,
            budget: std::cell::Cell::new(4096),
        };
        let mut pos = 0;
        while pos < toks.len() {
            let end = toks[pos..]
                .iter()
                .position(|t| *t == Tok::Nl)
                .map_or(toks.len(), |p| pos + p);
            if pos < end {
                e.stmts.push((pos, end));
            }
            e.read_statement(pos, end);
            pos = end + 1;
        }
        e
    }

    /// Mark the plain name `n` (or the module a dotted import binds) rebound;
    /// `param` when the binding is a `def` parameter.
    fn rebind(&mut self, n: &str, param: bool, import: bool) {
        self.rebound.insert(top(n).to_string());
        if import {
            self.imported.insert(top(n).to_string());
        } else if !param && !n.contains('.') {
            self.nonparam.insert(n.to_string());
        }
    }

    fn read_statement(&mut self, pos: usize, end: usize) {
        let toks = self.toks;
        let st = &toks[pos..end];
        let loop_targets = values::simple_for_header(st);
        if let Some((targets, iter)) = &loop_targets {
            for (i, n) in targets.iter().enumerate() {
                self.loops.entry(n.clone()).or_default().push((
                    i,
                    targets.len(),
                    (pos + iter.0, pos + iter.1),
                ));
            }
        }
        let params = values::def_param_positions(st);
        let mut depth = 0usize;
        let mut eqs = Vec::new();
        for (k, t) in st.iter().enumerate() {
            if is_assign_eq(st, k, depth) {
                eqs.push(k);
            }
            match t {
                Tok::Punct('(' | '[' | '{') => depth += 1,
                Tok::Punct(')' | ']' | '}') => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
        let simple_lhs = name(st.first()).filter(|n| !n.contains('.'));
        if let (Some(lhs), [1]) = (simple_lhs, eqs.as_slice()) {
            self.assigns
                .entry(lhs.to_string())
                .or_default()
                .push((pos + 2, end));
        } else if let Some(&last) = eqs.last() {
            // Any other assignment: every name in its targets is rebound.
            let names: Vec<String> = st[..last]
                .iter()
                .filter_map(|t| name(Some(t)).map(str::to_string))
                .collect();
            for n in names {
                self.rebind(&n, false, false);
            }
        }
        let whole_statement = matches!(
            name(st.first()),
            Some("def" | "class" | "global" | "nonlocal" | "del" | "import" | "from" | "case")
        );
        let import = matches!(name(st.first()), Some("import" | "from"));
        let mut marks: Vec<(String, bool)> = Vec::new();
        let mut until: Option<&str> = None;
        for (k, t) in st.iter().enumerate() {
            let Some(n) = name(Some(t)) else {
                if until == Some(":") && *t == Tok::Punct(':') {
                    until = None;
                }
                continue;
            };
            let next = st.get(k + 1);
            let walrus = next == Some(&Tok::Punct(':')) && st.get(k + 2) == Some(&Tok::Punct('='));
            let annotated = k == 0 && next == Some(&Tok::Punct(':'));
            let augmented = k == 0
                && matches!(next, Some(Tok::Punct(c)) if *c != '=')
                && st.get(k + 2) == Some(&Tok::Punct('='));
            let after_as = k > 0 && name(st.get(k - 1)) == Some("as");
            // A target of a recognised simple `for` header is read through
            // `loops` instead (only those name tokens, between `for` and `in`).
            let loop_target = until == Some("in")
                && loop_targets
                    .as_ref()
                    .is_some_and(|(ts, iter)| k < iter.0 && ts.iter().any(|x| x == n));
            let closes_for = n == "in" && until == Some("in");
            if (whole_statement || walrus || annotated || augmented || after_as || until.is_some())
                && !loop_target
                && !closes_for
            {
                marks.push((n.to_string(), params.contains(&k)));
            }
            match n {
                "for" => until = Some("in"),
                "lambda" => until = Some(":"),
                "in" if closes_for => until = None,
                _ => {}
            }
        }
        for (n, param) in marks {
            self.rebind(&n, param, import);
        }
    }

    /// The index of the token that closes the bracket opened at `open`.
    fn close(&self, open: usize) -> usize {
        let mut depth = 0usize;
        for (k, t) in self.toks.iter().enumerate().skip(open) {
            match t {
                Tok::Punct('(' | '[' | '{') => depth += 1,
                Tok::Punct(')' | ']' | '}') => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return k;
                    }
                }
                _ => {}
            }
        }
        self.toks.len()
    }

    /// The argument spans of the call whose `(` is at `open`.
    fn args(&self, open: usize) -> Vec<(usize, usize)> {
        let close = self.close(open);
        let mut out = Vec::new();
        let mut depth = 0usize;
        let mut start = open + 1;
        for k in open + 1..close {
            match &self.toks[k] {
                Tok::Punct('(' | '[' | '{') => depth += 1,
                Tok::Punct(')' | ']' | '}') => depth = depth.saturating_sub(1),
                Tok::Punct(',') if depth == 0 => {
                    out.push((start, k));
                    start = k + 1;
                }
                _ => {}
            }
        }
        if start < close {
            out.push((start, close));
        }
        out
    }

    fn keyword(&self, span: (usize, usize)) -> Option<&str> {
        match (self.toks.get(span.0), self.toks.get(span.0 + 1)) {
            (Some(Tok::Name { text, .. }), Some(Tok::Punct('=')))
                if self.toks.get(span.0 + 2) != Some(&Tok::Punct('=')) =>
            {
                Some(text)
            }
            _ => None,
        }
    }

    /// The positional argument `n` of a call, or its keyword `kw`.
    fn arg(&self, open: usize, n: usize, kw: &str) -> Option<(usize, usize)> {
        let args = self.args(open);
        if let Some(a) = args.iter().find(|a| self.keyword(**a) == Some(kw)) {
            return Some((a.0 + 2, a.1));
        }
        args.into_iter()
            .filter(|a| self.keyword(*a).is_none())
            .nth(n)
    }

    /// A string literal (implicit concatenation included) spanning exactly
    /// `[s, e)`, or `None`.
    fn literal(&self, s: usize, e: usize) -> Option<String> {
        if s >= e {
            return None;
        }
        let mut out = String::new();
        for t in &self.toks[s..e] {
            match t {
                Tok::Str { text, fstring } if !(*fstring && text.contains('{')) => {
                    out.push_str(text)
                }
                _ => return None,
            }
        }
        Some(out)
    }

    /// Every path the expression `[s, e)` may name, or `None` when it cannot
    /// be read statically.
    fn expr(&self, s: usize, e: usize) -> Option<Vals> {
        let (mut v, mut k) = self.chain(s, e)?;
        while k < e {
            let op = self.toks.get(k)?;
            let (rhs, next) = self.chain(k + 1, e)?;
            v = match op {
                Tok::Punct('/') => cross(&v, &rhs, PathVal::join)?,
                Tok::Punct('+') => cross(&v, &rhs, PathVal::concat)?,
                _ => return None,
            };
            k = next;
        }
        Some(v)
    }

    /// A primary followed by attribute calls, read up to the first binary
    /// operator; the values and the index after it.
    fn chain(&self, s: usize, e: usize) -> Option<(Vals, usize)> {
        let mut k = s;
        let mut v = match self.toks.get(k)? {
            Tok::Str { .. } => {
                let mut j = k;
                while j < e && matches!(self.toks[j], Tok::Str { .. }) {
                    j += 1;
                }
                let lit = self.literal(k, j)?;
                k = j;
                vec![PathVal::Lit(lit)]
            }
            Tok::Punct('(') => {
                let c = self.close(k);
                let inner = self.expr(k + 1, c)?;
                k = c + 1;
                inner
            }
            Tok::Name { text, .. } => {
                if let Some((v, next)) = self.argv_subscript(k) {
                    k = next;
                    v
                } else if self.toks.get(k + 1) == Some(&Tok::Punct('(')) {
                    let v = self.call_value(text, k + 1)?;
                    k = self.close(k + 1) + 1;
                    v
                } else {
                    k += 1;
                    self.name_value_at(text, k - 1)?
                }
            }
            _ => return None,
        };
        while k < e && self.toks[k] == Tok::Punct('.') {
            let m = name(self.toks.get(k + 1))?;
            if self.toks.get(k + 2) != Some(&Tok::Punct('(')) {
                return None;
            }
            v = self.method_value(v, m, k + 2)?;
            k = self.close(k + 2) + 1;
        }
        Some((v, k))
    }

    /// The values of the plain name `n` ([`Eval`]): the union over every
    /// `n = …` statement, `None` when `n` is rebound or unassigned.
    fn name_value(&self, n: &str) -> Option<Vals> {
        if self.rebound.contains(n) {
            return None;
        }
        let left = self.budget.get().checked_sub(1)?;
        self.budget.set(left);
        let (assigns, loops) = (self.assigns.get(n), self.loops.get(n));
        if assigns.is_none() && loops.is_none() {
            return None;
        }
        let mut out = Vec::new();
        for (s, e) in assigns.into_iter().flatten() {
            out.extend(self.expr(*s, *e)?);
            if out.len() > MAX_VALUES {
                return None;
            }
        }
        for (index, count, iter) in loops.into_iter().flatten() {
            out.extend(self.loop_value(*index, *count, *iter)?);
            if out.len() > MAX_VALUES {
                return None;
            }
        }
        Some(out)
    }

    /// [`Eval::name_value`], except that `self.attr` read at token `k` is
    /// resolved through its class ([`Eval::attr_value`]).
    fn name_value_at(&self, n: &str, k: usize) -> Option<Vals> {
        match n.strip_prefix("self.") {
            Some(attr) if !attr.contains('.') => self.attr_value(attr, k),
            _ => self.name_value(n),
        }
    }

    fn call_value(&self, n: &str, open: usize) -> Option<Vals> {
        let arg0 = || self.arg(open, 0, "").and_then(|(s, e)| self.expr(s, e));
        let one = |v: PathVal| Some(vec![v]);
        match n {
            "os.path.expanduser" | "posixpath.expanduser" => {
                arg0()?.into_iter().map(PathVal::expanduser).collect()
            }
            "os.path.join" | "posixpath.join" => self.join_args(open, PathVal::Lit(String::new())),
            "os.path.abspath" | "os.path.normpath" | "os.path.realpath" | "str" | "os.fspath" => {
                arg0()
            }
            "pathlib.Path" | "pathlib.PurePath" | "pathlib.PosixPath" | "pathlib.PurePosixPath" => {
                self.join_args(open, PathVal::Lit(".".to_string()))
            }
            "pathlib.Path.home" | "pathlib.PosixPath.home" => one(PathVal::Home(String::new())),
            "pathlib.Path.cwd" | "pathlib.PosixPath.cwd" | "os.getcwd" => {
                one(PathVal::Lit(".".to_string()))
            }
            _ => {
                let (recv, m) = n.rsplit_once('.')?;
                let recv = self.name_value_at(recv, open.checked_sub(1)?)?;
                self.method_value(recv, m, open)
            }
        }
    }

    fn join_args(&self, open: usize, init: PathVal) -> Option<Vals> {
        let mut v = vec![init];
        for (s, e) in self.args(open) {
            v = cross(&v, &self.expr(s, e)?, PathVal::join)?;
        }
        Some(v)
    }

    fn method_value(&self, recv: Vals, m: &str, open: usize) -> Option<Vals> {
        match m {
            "joinpath" => {
                let mut out = Vec::new();
                for r in recv {
                    out.extend(self.join_args(open, r)?);
                }
                (out.len() <= MAX_VALUES).then_some(out)
            }
            "expanduser" => recv.into_iter().map(PathVal::expanduser).collect(),
            "resolve" | "absolute" | "__fspath__" => Some(recv),
            _ => None,
        }
    }

    /// The receiver of the method call named by token `k`: the name part
    /// before the method of a dotted name (`p.unlink`), or the chain that
    /// ends at the `.` before token `k` (`Path.home().joinpath(x).unlink`).
    fn receiver(&self, k: usize, n: &str) -> Option<Vals> {
        if k > 0 && self.toks[k - 1] == Tok::Punct('.') {
            let start = self.chain_start(k - 1)?;
            let (v, end) = self.chain(start, k - 1)?;
            return (end == k - 1).then_some(v);
        }
        let (recv, _) = n.rsplit_once('.')?;
        self.name_value_at(recv, k)
    }

    /// The first token of the postfix chain that ends just before `dot`.
    fn chain_start(&self, dot: usize) -> Option<usize> {
        let mut k = dot;
        loop {
            let prev = k.checked_sub(1)?;
            let start = match &self.toks[prev] {
                Tok::Punct(')') => {
                    let open = self.open_of(prev)?;
                    match open.checked_sub(1).map(|p| &self.toks[p]) {
                        Some(Tok::Name { .. }) => open - 1,
                        _ => open,
                    }
                }
                Tok::Name { .. } | Tok::Str { .. } => prev,
                _ => return None,
            };
            if start > 0 && self.toks[start - 1] == Tok::Punct('.') {
                k = start - 1;
            } else {
                return Some(start);
            }
        }
    }

    fn open_of(&self, close: usize) -> Option<usize> {
        let mut depth = 0usize;
        for k in (0..=close).rev() {
            match self.toks[k] {
                Tok::Punct(')' | ']' | '}') => depth += 1,
                Tok::Punct('(' | '[' | '{') => {
                    depth = depth.checked_sub(1)?;
                    if depth == 0 {
                        return Some(k);
                    }
                }
                _ => {}
            }
        }
        None
    }
}

/// What a filesystem call does to a path, as the shell command with the same
/// effect.
#[derive(Clone, Copy)]
enum Effect {
    /// `rm [-rf] <path>`.
    Remove { recursive: bool },
    /// `: > <path>` (truncate) or `: >> <path>` (append / update in place).
    Write { append: bool },
    /// `chmod <mode> <path>`: `os.chmod` / `os.lchmod` / `Path.chmod()` /
    /// `Path.lchmod()`. `mode` is the value of a Python integer-literal mode;
    /// any other mode is rendered as `000`, which removes read and exec (the
    /// restrictive reading of the shell `chmod` rule).
    Chmod { mode: Option<u32> },
    /// `chown 0:0 <path>`: `os.chown` / `os.lchown` / `shutil.chown`.
    Chown,
}

impl Effect {
    /// The shell command with this effect on `target` (an already quoted
    /// shell word, or a glob such as `dir/*`).
    fn shell_line(self, target: &str) -> String {
        match self {
            Effect::Remove { recursive: true } => format!("rm -rf {target}"),
            Effect::Remove { recursive: false } => format!("rm {target}"),
            Effect::Write { append: false } => format!(": > {target}"),
            Effect::Write { append: true } => format!(": >> {target}"),
            Effect::Chmod { mode } => format!("chmod {:03o} {target}", mode.unwrap_or(0)),
            Effect::Chown => format!("chown 0:0 {target}"),
        }
    }
}

/// The value of a Python integer literal (`0`, `0o755`, `0x1ff`, `0b111`,
/// `493`, underscores allowed), `None` for anything else.
fn int_literal(text: &str) -> Option<u32> {
    let lower = text.replace('_', "").to_ascii_lowercase();
    let (digits, radix) = match lower.get(..2) {
        Some("0o") => (&lower[2..], 8),
        Some("0x") => (&lower[2..], 16),
        Some("0b") => (&lower[2..], 2),
        _ => (lower.as_str(), 10),
    };
    if digits.is_empty() || (radix == 10 && digits.len() > 1 && digits.starts_with('0')) {
        return None;
    }
    u32::from_str_radix(digits, radix).ok()
}

/// The mode of a chmod call: its argument when that is exactly one integer
/// literal, `None` otherwise.
fn literal_chmod_mode(ev: &Eval<'_>, span: Option<(usize, usize)>) -> Option<u32> {
    let (s, e) = span?;
    if e != s + 1 {
        return None;
    }
    match ev.toks.get(s)? {
        Tok::Name { text, .. } => int_literal(text),
        _ => None,
    }
}

/// A literal `open` mode, `None` when the argument is not one literal mode.
fn literal_mode(ev: &Eval<'_>, span: Option<(usize, usize)>) -> Option<Option<Effect>> {
    let Some((s, e)) = span else {
        return Some(None);
    };
    let m = ev.literal(s, e)?;
    if m.is_empty() || !m.chars().all(|c| "rwxabtU+".contains(c)) {
        // Not a mode string: `tarfile.open("x.tar")`, `webbrowser.open(url)`.
        return Some(None);
    }
    Some(if m.contains(['w', 'x']) {
        Some(Effect::Write { append: false })
    } else if m.contains(['a', '+']) {
        Some(Effect::Write { append: true })
    } else {
        None
    })
}

/// The verdict on the paths `src` (rewritten) removes or writes:
/// `shutil.rmtree(p)` as `rm -rf p`; `os.remove` / `os.unlink` / `os.rmdir` /
/// `os.removedirs` and `Path.unlink()` / `Path.rmdir()` as `rm p`; the
/// destination of `shutil.copy` / `copy2` / `copyfile` / `copytree` / `move`
/// and `os.rename` / `replace` / `renames` as `: > dst` (the source a move
/// removes is not judged); `os.truncate(p, …)` as `: > p`; `os.chmod` /
/// `os.lchmod` and `Path.chmod()` / `Path.lchmod()` as `chmod <mode> p`
/// (an integer-literal mode as written, any other mode as `000`);
/// `os.chown` / `os.lchown` / `shutil.chown` as `chown 0:0 p`;
/// `open(p, 'w'|'x'|…)`, `Path.open('w')`, `Path.write_text` / `write_bytes`
/// as `: > p`, and an append or `+` mode as `: >> p`; `os.open` with a write
/// flag likewise; `Path.touch()` as `: >> p` and `Path.symlink_to` /
/// `hardlink_to` as `: > p`; a ONE-argument `.replace(dst)` / `.rename(dst)`
/// (positional or `target=`: `Path.replace` / `Path.rename`, never the
/// two-argument `str.replace`) as `: > dst` plus `rm` of its receiver
/// (backlog 17d985f1). The path of a method is its RECEIVER, resolved by
/// [`Eval::receiver`] through `/`, `joinpath` and parentheses. Each command is judged by the shell rules, so home, `/` and
/// dotfile targets get the shell's Deny/Ask and a repo-relative path stays
/// Allow. A path computed by iterating a repo-relative directory, or read
/// from `sys.argv` / a constructor argument, is judged by [`values`] (user
/// ruling 2026-11-08: Allow only when provably repo-scoped). A path that
/// cannot be resolved statically ([`Eval::expr`]), or an `open` mode /
/// `os.open` flag that is not a literal, is an Ask. `argv` is the arguments
/// of every run of the file ([`values::Argv`]), `None` when not known.
///
/// A path is RESOLVED, not matched: `'/' 'etc/hosts'` (Python's implicit
/// concatenation of adjacent literals), `'/etc/' + 'hosts'` and
/// `os.path.join('/etc', 'hosts')` are all `/etc/hosts` here (backlog
/// f6dbff5b).
pub(super) fn filesystem_verdict(
    src: &str,
    argv: Option<&values::Argv>,
    depth: usize,
    ctx: &Ctx<'_>,
) -> Decision {
    path_calls_verdict(src, argv, depth, ctx)
}

/// [`filesystem_verdict`] for an inline Python program (`python -c`, or a
/// program `python` reads from stdin), every call it lists included — the
/// `open` / `os.open` / `.open()` / `write_text` / `write_bytes` writes too
/// (backlog 17d985f1; they are ALSO judged by the interpreter write-target
/// scan, which reads only literal tokens). `src` is read with its import
/// aliases resolved ([`bindings`] / [`rewrite`]: `from os import chmod`).
///
/// Backlog f6dbff5b: the literal scan of [`super::shell_calls`] sees only the
/// string tokens of a call, so `os.chmod('/''etc/hosts', 0)` — two tokens,
/// neither in a system directory — was Allow. Here the path argument is
/// evaluated ([`Eval::expr`]) and the resolved value judged by the shell
/// rules; a path argument that cannot be resolved statically is an Ask
/// (Deny where no human can answer), never Allow. Backlog 17d985f1: the same
/// holds for the RECEIVER of a pathlib write method, so
/// `(Path('/etc') / 'hosts').write_text('x')` and
/// `Path('/etc').joinpath('hosts').write_text('x')` are writes of
/// `/etc/hosts`, and for the DESTINATION of `Path.replace` / `Path.rename`
/// (`Path('/tmp/y').replace('/etc/hosts')`). Backlog c8bf27ea: the same
/// callables reached other than by a direct call — the unbound
/// `Path.replace(Path('/tmp/z'), '/etc/hosts')`, `w = Path(p).write_text;
/// w('x')`, `.write_text.__call__('x')`, a reference passed or stored — are
/// judged too ([`method_refs`]); an unresolvable one is an Ask.
///
/// Backlog 67219006: a write whose target cannot be DETERMINED by the shell
/// redirect rule — a relative path with no known base directory (the payload
/// has no `cwd`), or one carrying `$` / a backtick — is a pending Ask here,
/// not the Deny the redirect rule gives a real shell `> a/b`. The resolved
/// value is judged through the same R2 side channel the program-text shell
/// pass uses ([`super::ProgramTextSink`], `redirects_are_data: false`): the
/// redirect rule records such a target instead of denying it, and this
/// function turns the record into an Ask. That is the outcome the interpreter
/// write-target scan already gives the same literal (`Recovery::Undetermined`
/// is a pending Ask, backlog 7483e85b). Every other refusal of the redirect
/// rule (system directory, protected path, an existing unrecoverable target)
/// still denies, a Deny earned anywhere else in the program still outranks
/// the Ask, and with no human to answer (a subagent) the Ask hardens to Deny.
pub(super) fn path_mutation_verdict(src: &str, depth: usize, ctx: &Ctx<'_>) -> Decision {
    let rewritten = rewrite(src, &bindings(src));
    let undeterminable = super::ProgramTextSink {
        rows: std::cell::RefCell::new(Vec::new()),
        redirects_are_data: false,
    };
    let sink_ctx = Ctx {
        program_text_redirects: Some(&undeterminable),
        ..ctx.clone()
    };
    match path_calls_verdict(&rewritten, None, depth, &sink_ctx) {
        d @ Decision::Deny(_) => d,
        a @ Decision::Ask(_) => a,
        Decision::Allow => match undeterminable.rows.into_inner().into_iter().next() {
            Some(row) => Decision::ask(format!(
                "the program writes a path whose location blastguard cannot determine: {row}"
            )),
            None => Decision::Allow,
        },
    }
}

/// Judge every value in `path` (the resolved target of the call `n`) as the
/// shell command with `effect` on it. A Deny is returned at once; an Ask is
/// recorded in `pending` (the first one stands). `None` — a target that
/// cannot be resolved statically — is an Ask, never an Allow.
fn judge_paths(
    n: &str,
    effect: Effect,
    path: Option<Vals>,
    depth: usize,
    ctx: &Ctx<'_>,
    pending: &mut Option<Decision>,
) -> Option<Decision> {
    let Some(paths) = path else {
        pending.get_or_insert(Decision::ask(format!(
            "the program calls `{n}` on a path blastguard cannot resolve statically — it \
cannot tell whether that removes, overwrites or re-permissions something irreversible"
        )));
        return None;
    };
    for path in paths {
        let Some(word) = path.shell_word() else {
            match values::under_verdict(n, &path, effect, depth, ctx) {
                d @ Decision::Deny(_) => return Some(d),
                a @ Decision::Ask(_) => {
                    pending.get_or_insert(a);
                }
                Decision::Allow => {}
            }
            continue;
        };
        let line = effect.shell_line(&word);
        match analyze_shell_payload(&line, depth, ctx) {
            d @ Decision::Deny(_) => return Some(d),
            Decision::Ask(r) => {
                pending.get_or_insert(Decision::ask(format!(
                    "the program calls `{n}`, which does what `{line}` does: {r}"
                )));
            }
            Decision::Allow => {}
        }
    }
    None
}

/// The calls [`filesystem_verdict`] and [`path_mutation_verdict`] judge.
///
/// Backlog c8bf27ea: a call is read through [`method_refs::callees`], so
/// `X.m.__call__(…)` is `X.m(…)`, the unbound `Path.m(recv, …)` is
/// `recv.m(…)`, and `w(…)` for `w = X.m` (every binding of `w` such a
/// reference) is `X.m(…)`. A write/move-capable callable referenced any other
/// way — stored, passed, put in a container, reached through `getattr` /
/// `methodcaller` — is judged by [`method_refs::escaped_reference_verdict`].
fn path_calls_verdict(
    src: &str,
    argv: Option<&values::Argv>,
    depth: usize,
    ctx: &Ctx<'_>,
) -> Decision {
    let toks = lex(src);
    let ev = Eval::new(&toks, src, argv);
    let mut pending: Option<Decision> = None;
    for (k, t) in toks.iter().enumerate() {
        let Tok::Name { text, .. } = t else {
            continue;
        };
        if toks.get(k + 1) != Some(&Tok::Punct('(')) {
            continue;
        }
        let attr = k > 0 && toks[k - 1] == Tok::Punct('.');
        for call in method_refs::callees(&ev, k, text, attr) {
            if let Some(d) = judge_call(&ev, &call, depth, ctx, &mut pending) {
                return d;
            }
        }
    }
    if let Some(d) = method_refs::escaped_reference_verdict(&ev, depth, ctx, &mut pending) {
        return d;
    }
    pending.unwrap_or(Decision::Allow)
}

/// Judge one resolved call ([`method_refs::Call`]); a Deny is returned, an Ask
/// recorded in `pending`.
fn judge_call(
    ev: &Eval<'_>,
    call: &method_refs::Call<'_>,
    depth: usize,
    ctx: &Ctx<'_>,
    pending: &mut Option<Decision>,
) -> Option<Decision> {
    let toks = ev.toks;
    let (n, attr) = (call.n, call.attr);
    let method = n.rsplit('.').next().unwrap_or(n);
    let path_arg = |idx: usize, kw: &str| call.arg(ev, idx, kw).and_then(|(s, e)| ev.expr(s, e));
    let (effect, path) = match n {
        "shutil.rmtree" if !attr => (Effect::Remove { recursive: true }, path_arg(0, "path")),
        "os.remove" | "os.unlink" | "os.rmdir" | "os.removedirs" if !attr => {
            (Effect::Remove { recursive: false }, path_arg(0, "path"))
        }
        // The destination of a copy or a move is overwritten, as by
        // `open(dst, 'w')` (tests/heredoc_system_harm_holes_20261111.rs B).
        "shutil.copy" | "shutil.copy2" | "shutil.copyfile" | "shutil.copytree" | "shutil.move"
        | "os.rename" | "os.replace" | "os.renames"
            if !attr =>
        {
            (Effect::Write { append: false }, path_arg(1, "dst"))
        }
        "os.truncate" if !attr => (Effect::Write { append: false }, path_arg(0, "path")),
        "os.chmod" | "os.lchmod" if !attr => (
            Effect::Chmod {
                mode: literal_chmod_mode(ev, call.arg(ev, 1, "mode")),
            },
            path_arg(0, "path"),
        ),
        "os.chown" | "os.lchown" | "shutil.chown" if !attr => (Effect::Chown, path_arg(0, "path")),
        "open" | "io.open" | "codecs.open" | "builtins.open" if !attr => {
            match literal_mode(ev, call.arg(ev, 1, "mode")) {
                Some(Some(eff)) => (eff, path_arg(0, "file")),
                Some(None) => return None,
                None => {
                    pending.get_or_insert(computed(n, "opens a file in a mode"));
                    return None;
                }
            }
        }
        "os.open" if !attr => {
            let flags: Vec<&str> = call
                .arg(ev, 1, "flags")
                .map(|(s, e)| toks[s..e].iter().filter_map(|t| name(Some(t))).collect())
                .unwrap_or_default();
            if flags.iter().any(|f| !f.starts_with("os.O_")) {
                pending.get_or_insert(computed(n, "opens a file with flags"));
                return None;
            }
            let has = |f: &str| flags.iter().any(|x| x.ends_with(f));
            if !(has("O_WRONLY") || has("O_RDWR") || has("O_CREAT") || has("O_TRUNC")) {
                return None;
            }
            (
                Effect::Write {
                    append: !has("O_TRUNC"),
                },
                path_arg(0, "path"),
            )
        }
        _ if attr || n.contains('.') => {
            let effect = match method {
                "unlink" | "rmdir" => Effect::Remove { recursive: false },
                "chmod" | "lchmod" => Effect::Chmod {
                    mode: literal_chmod_mode(ev, call.arg(ev, 0, "mode")),
                },
                "write_text" | "write_bytes" => Effect::Write { append: false },
                // Create or update the receiver path (backlog 17d985f1).
                "touch" => Effect::Write { append: true },
                "symlink_to" | "hardlink_to" => Effect::Write { append: false },
                // `Path.replace(target)` / `Path.rename(target)` overwrite
                // the DESTINATION and move the receiver away (backlog
                // 17d985f1). Only the one-argument shape: `str.replace`
                // takes two or three, and a keyword other than `target`
                // (`dt.replace(year=…)`) is not this call.
                "replace" | "rename" => {
                    let args = call.args(ev);
                    let one =
                        args.len() == 1 && ev.keyword(args[0]).is_none_or(|kw| kw == "target");
                    if !one {
                        return None;
                    }
                    let dst = path_arg(0, "target");
                    let write = Effect::Write { append: false };
                    if let Some(d) = judge_paths(n, write, dst, depth, ctx, pending) {
                        return Some(d);
                    }
                    Effect::Remove { recursive: false }
                }
                // A non-literal first argument of an `.open` on a value
                // that is not a path (`webbrowser.open(url)`,
                // `tarfile.open(name)`) is not a mode; on a path it is.
                "open" => match literal_mode(ev, call.arg(ev, 0, "mode")) {
                    Some(Some(eff)) => eff,
                    Some(None) => return None,
                    None if call.receiver(ev).is_none() => return None,
                    None => {
                        pending.get_or_insert(computed(n, "opens a file in a mode"));
                        return None;
                    }
                },
                _ => return None,
            };
            (effect, call.receiver(ev))
        }
        _ => return None,
    };
    judge_paths(n, effect, path, depth, ctx, pending)
}

fn computed(call: &str, what: &str) -> Decision {
    Decision::ask(format!(
        "the program calls `{call}`, which {what} blastguard cannot read statically — it cannot \
tell whether that overwrites something irreversible"
    ))
}

/// DB-API driver modules: a program importing one runs SQL through
/// `.execute(…)` and friends.
const DB_MODULES: &[&str] = &[
    "sqlite3",
    "apsw",
    "duckdb",
    "psycopg",
    "psycopg2",
    "pymysql",
    "MySQLdb",
    "mysql",
    "pg8000",
    "asyncpg",
    "aiosqlite",
];

/// SQL statement words that destroy or rewrite stored data.
const DESTRUCTIVE_SQL: &[&str] = &["drop", "delete", "truncate", "alter", "update", "replace"];

/// The SQL rule, scoped to programs that import a DB-API driver
/// ([`DB_MODULES`]): an `.execute` / `.executemany` / `.executescript` call
/// whose SQL is a literal holding a [`DESTRUCTIVE_SQL`] word is an Ask, and so
/// is one whose SQL is not a literal (the statement only exists at run time).
/// A program that imports no driver is never judged here.
pub(super) fn sql_verdict(src: &str, b: &Bindings) -> Option<Decision> {
    if !b.imported.iter().any(|m| DB_MODULES.contains(&m.as_str())) {
        return None;
    }
    let toks = lex(src);
    let ev = Eval::new(&toks, src, None);
    for (k, t) in toks.iter().enumerate() {
        let Some(n) = name(Some(t)) else { continue };
        let method = n.rsplit('.').next().unwrap_or(n);
        if !matches!(method, "execute" | "executemany" | "executescript")
            || toks.get(k + 1) != Some(&Tok::Punct('('))
        {
            continue;
        }
        let sql = ev.arg(k + 1, 0, "sql").and_then(|(s, e)| ev.literal(s, e));
        let Some(sql) = sql else {
            return Some(Decision::ask(format!(
                "the program runs SQL it computes through `{n}` — blastguard cannot see whether \
it drops or deletes stored data"
            )));
        };
        let lowered = sql.to_ascii_lowercase();
        let words: HashSet<&str> = lowered
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .collect();
        if let Some(w) = DESTRUCTIVE_SQL.iter().find(|w| words.contains(**w)) {
            return Some(Decision::ask(format!(
                "the program runs SQL with `{}` through `{n}` — that destroys or rewrites stored \
data, which blastguard cannot undo",
                w.to_ascii_uppercase()
            )));
        }
    }
    None
}

/// The import rule: loading a module runs that module's code, which is not in
/// this text. A relative import (`from . import x`), a `from <runner> import *`
/// (the names it binds are not visible), or an absolute import of a module
/// that is not in the standard library ([`STDLIB`]) is an Ask: the module may
/// be a sibling file written by another command. Standard-library imports —
/// what an ordinary edit script uses — stay Allow.
pub(super) fn import_verdict(b: &Bindings) -> Option<Decision> {
    if b.relative {
        return Some(Decision::ask(
            "the program does a relative import — the imported module's code is not in this \
text, so blastguard cannot vouch for what it does",
        ));
    }
    if let Some(m) = &b.star_runner {
        return Some(Decision::ask(format!(
            "the program does `from {m} import *` — the names it binds run commands or remove \
files and cannot be followed, so blastguard cannot vouch for it"
        )));
    }
    let foreign = b.imported.iter().find(|m| !STDLIB.contains(&m.as_str()))?;
    Some(Decision::ask(format!(
        "the program imports `{foreign}`, which is not a standard-library module — its code (it \
may be a sibling file) is not in this text, so blastguard cannot vouch for what it does"
    )))
}

/// Top-level standard-library module names (CPython 3.13
/// `sys.stdlib_module_names`, private `_`-modules left out). A name missing
/// here only makes an import an Ask.
const STDLIB: &[&str] = &[
    "__future__",
    "abc",
    "aifc",
    "argparse",
    "array",
    "ast",
    "asynchat",
    "asyncio",
    "asyncore",
    "atexit",
    "audioop",
    "base64",
    "bdb",
    "binascii",
    "bisect",
    "builtins",
    "bz2",
    "cProfile",
    "calendar",
    "cgi",
    "cgitb",
    "chunk",
    "cmath",
    "cmd",
    "code",
    "codecs",
    "codeop",
    "collections",
    "colorsys",
    "compileall",
    "concurrent",
    "configparser",
    "contextlib",
    "contextvars",
    "copy",
    "copyreg",
    "crypt",
    "csv",
    "ctypes",
    "curses",
    "dataclasses",
    "datetime",
    "dbm",
    "decimal",
    "difflib",
    "dis",
    "doctest",
    "email",
    "encodings",
    "ensurepip",
    "enum",
    "errno",
    "faulthandler",
    "fcntl",
    "filecmp",
    "fileinput",
    "fnmatch",
    "fractions",
    "ftplib",
    "functools",
    "gc",
    "getopt",
    "getpass",
    "gettext",
    "glob",
    "graphlib",
    "grp",
    "gzip",
    "hashlib",
    "heapq",
    "hmac",
    "html",
    "http",
    "imaplib",
    "importlib",
    "inspect",
    "io",
    "ipaddress",
    "itertools",
    "json",
    "keyword",
    "linecache",
    "locale",
    "logging",
    "lzma",
    "mailbox",
    "marshal",
    "math",
    "mimetypes",
    "mmap",
    "modulefinder",
    "multiprocessing",
    "netrc",
    "numbers",
    "operator",
    "optparse",
    "os",
    "pathlib",
    "pdb",
    "pickle",
    "pickletools",
    "pkgutil",
    "platform",
    "plistlib",
    "poplib",
    "posix",
    "posixpath",
    "pprint",
    "profile",
    "pstats",
    "pty",
    "pwd",
    "py_compile",
    "pyclbr",
    "pydoc",
    "queue",
    "quopri",
    "random",
    "re",
    "readline",
    "reprlib",
    "resource",
    "rlcompleter",
    "runpy",
    "sched",
    "secrets",
    "select",
    "selectors",
    "shelve",
    "shlex",
    "shutil",
    "signal",
    "site",
    "smtplib",
    "socket",
    "socketserver",
    "sqlite3",
    "ssl",
    "stat",
    "statistics",
    "string",
    "stringprep",
    "struct",
    "subprocess",
    "symtable",
    "sys",
    "sysconfig",
    "syslog",
    "tabnanny",
    "tarfile",
    "tempfile",
    "termios",
    "textwrap",
    "threading",
    "time",
    "timeit",
    "tkinter",
    "token",
    "tokenize",
    "tomllib",
    "trace",
    "traceback",
    "tracemalloc",
    "tty",
    "turtle",
    "types",
    "typing",
    "unicodedata",
    "unittest",
    "urllib",
    "uuid",
    "venv",
    "warnings",
    "wave",
    "weakref",
    "webbrowser",
    "wsgiref",
    "xml",
    "xmlrpc",
    "zipapp",
    "zipfile",
    "zipimport",
    "zlib",
    "zoneinfo",
];

#[cfg(test)]
mod tests {
    use super::{int_literal, Effect};

    #[test]
    fn python_integer_literals_and_non_literals() {
        assert_eq!(int_literal("0"), Some(0));
        assert_eq!(int_literal("0o755"), Some(0o755));
        assert_eq!(int_literal("0O644"), Some(0o644));
        assert_eq!(int_literal("493"), Some(0o755));
        assert_eq!(int_literal("0x1ff"), Some(0o777));
        assert_eq!(int_literal("0b111"), Some(7));
        assert_eq!(int_literal("0o7_5_5"), Some(0o755));
        assert_eq!(int_literal("0755"), None);
        assert_eq!(int_literal("mode"), None);
        assert_eq!(int_literal("stat.S_IRWXU"), None);
    }

    #[test]
    fn chmod_without_a_literal_mode_is_rendered_restrictively() {
        assert_eq!(Effect::Chmod { mode: None }.shell_line("x"), "chmod 000 x");
        assert_eq!(
            Effect::Chmod { mode: Some(0o755) }.shell_line("x"),
            "chmod 755 x"
        );
        assert_eq!(Effect::Chown.shell_line("x"), "chown 0:0 x");
    }
}
