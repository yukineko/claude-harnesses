//! The Python reading: a program's text → its effects, or Undetermined.
//!
//! Pipeline: [`analyze`] normalises the source the way CPython's tokenizer
//! does (BOM, CR / CRLF newlines) and refuses what this reading cannot
//! follow (a NUL byte, a non-UTF-8 coding cookie, an oversized program, a
//! parse error), then runs two passes over one exhaustive traversal
//! ([`walk`]): the binding pass ([`bind`]) counts how every name is bound,
//! and the judging pass ([`judge`]) classifies every call.
//!
//! The rules the judging pass enforces (each "Undetermined" is a refusal to
//! guess, resolved by the host to Ask / Deny):
//!
//! - **Text vs code.** String literals, comments and the literal parts of
//!   f-strings are data. Only calls are effects.
//! - **Dynamic dispatch is Undetermined**: any reference to `exec`, `eval`,
//!   `compile`, `__import__`, `getattr`, `setattr`, `delattr`, `globals`,
//!   `locals`, `vars`, `breakpoint`, `help`, `__builtins__` (except a
//!   `globals()` / `locals()` / `vars(x)` / `compile(...)` call whose result
//!   is only displayed or discarded: a positional `print` argument or an
//!   expression statement, optionally subscripted — a discarded `compile`
//!   runs nothing whatever it compiles — and a bare unbound `globals` /
//!   `locals` / `vars` name that is itself a whole positional argument of the
//!   unbound builtin `print`, e.g. `print(vars)`; a starred, keyword, nested,
//!   attribute or aliased use of the bare name stays Undetermined); any dunder
//!   attribute other than `__name__` / `__doc__` / `__file__` /
//!   `__version__` / `__qualname__` (so `os.__dict__[…]` is Undetermined);
//!   `sys.modules`; an effect-capable function or module used as a value
//!   (`f = os.system`), since its later calls cannot be followed.
//! - **Aliasing**: an import alias (`import os as o`, `from os import system
//!   as s`) is followed when every binding of that name is an import of the
//!   same module / function. A name bound by an import AND some other way is
//!   Undetermined on use.
//! - **Imports**: only standard-library modules are read. Any other import
//!   (site-packages, a file of the working directory), a relative import and
//!   `from m import *` are Undetermined.
//! - **Unknown calls**: a call with no effect-capable name is no effect. A
//!   call into an effect-capable standard-library module that is not
//!   modelled (`os.fork`, `importlib.import_module`, `pickle.loads`, …) is
//!   Undetermined; a method on a receiver this reading does not resolve is
//!   Undetermined when its NAME runs a program or touches the file system
//!   ([`tables::EXEC_NAMES`], [`tables::FS_METHOD_NAMES`],
//!   [`tables::FILE_WRITER_NAMES`]).
//! - **Arguments**: an effect whose command / path is not determined by the
//!   program text (or by a literal `sys.argv` word) is Undetermined.

mod bind;
mod judge;
mod tables;
mod value;
mod walk;

use std::panic::{catch_unwind, AssertUnwindSafe};

use rustpython_parser::Parse;

use crate::Analysis;

/// Programs longer than this (bytes) are Undetermined rather than parsed.
pub const MAX_SOURCE_BYTES: usize = 16 * 1024;

/// Read a Python program.
///
/// `argv` is `sys.argv` as the interpreter will see it (`['-c', …]` for
/// `python3 -c`, `['-', …]` / `['']` for a program on stdin); an element is
/// `None` when the host could not determine that word.
pub fn analyze(source: &str, argv: &[Option<String>]) -> Analysis {
    let (source, body) = match parse(source) {
        Ok(p) => p,
        Err(why) => return Analysis::Undetermined(why),
    };
    let b = bind::Bindings::collect(&body);
    if let Some(why) = b.failure.clone() {
        return Analysis::Undetermined(why);
    }
    let mut j = judge::Judge::new(&b, argv, &source);
    walk::module(&mut j, &body);
    match j.failure {
        Some(why) => Analysis::Undetermined(why),
        None => Analysis::Known(j.effects),
    }
}

/// The top-level name of every module the program imports (`import a.b` /
/// `import a.b as x` / `from a.b import c` → `a`), wherever the import
/// statement sits (a function body, an unreachable branch), sorted and
/// deduplicated; `Err(why)` when the program is not read (the same refusals
/// as [`analyze`]: oversized, NUL, a non-UTF-8 coding cookie, a parse error,
/// a relative import, a non-ASCII identifier, too deep).
///
/// User ruling 2026-11-19 (blastguard S5b a): `python -c` / a program on
/// stdin puts the working directory first on `sys.path`, so a file `a.py` or
/// a directory `a/` there replaces the module `a` the program imports. The
/// host lists the program's directory for these names; this crate does not
/// touch the file system. Only the names the program itself imports are
/// listed — the modules those modules import in turn are not.
pub fn imported_modules(source: &str) -> Result<Vec<String>, String> {
    struct Imports {
        names: std::collections::BTreeSet<String>,
        failure: Option<String>,
    }
    impl walk::Visit for Imports {
        fn bind(&mut self, _name: &str, how: walk::Bind<'_>) {
            if let walk::Bind::Import(q) = how {
                let top = q.split('.').next().unwrap_or(&q).to_string();
                self.names.insert(top);
            }
        }
        fn fail(&mut self, why: String) {
            self.failure.get_or_insert(why);
        }
    }
    let (_, body) = parse(source)?;
    let mut v = Imports {
        names: std::collections::BTreeSet::new(),
        failure: None,
    };
    walk::module(&mut v, &body);
    match v.failure {
        Some(why) => Err(why),
        None => Ok(v.names.into_iter().collect()),
    }
}

/// The source text of every expression statement that is only names,
/// attributes, numbers and arithmetic operators (a shell line such as
/// `ls -la /tmp` parses as `ls - la / tmp`), wherever the statement sits;
/// `Err(why)` when the program is not read (the refusals of [`analyze`]).
///
/// Such a statement computes a value that is discarded and calls nothing, so
/// as Python it is inert. Real programs do not contain it; shell text pasted
/// into a Python program does, and this reading cannot tell whether the
/// author meant it as Python. A host that must not let shell text pass as an
/// inert program judges these statements' text as shell as well.
pub fn bare_operator_statements(source: &str) -> Result<Vec<String>, String> {
    struct Bare<'s> {
        source: &'s str,
        found: Vec<String>,
        failure: Option<String>,
    }
    fn only_names_and_operators(e: &rustpython_parser::ast::Expr, ops: &mut usize) -> bool {
        use rustpython_parser::ast::{Constant, Expr};
        match e {
            Expr::Name(_) => true,
            Expr::Attribute(a) => only_names_and_operators(&a.value, ops),
            Expr::Constant(c) => matches!(
                c.value,
                Constant::Int(_) | Constant::Float(_) | Constant::Complex { .. }
            ),
            Expr::BinOp(b) => {
                *ops += 1;
                only_names_and_operators(&b.left, ops) && only_names_and_operators(&b.right, ops)
            }
            Expr::UnaryOp(u) => {
                *ops += 1;
                only_names_and_operators(&u.operand, ops)
            }
            _ => false,
        }
    }
    impl walk::Visit for Bare<'_> {
        fn discarded(&mut self, e: &rustpython_parser::ast::Expr) {
            use rustpython_parser::ast::Ranged;
            let mut ops = 0;
            if only_names_and_operators(e, &mut ops) && ops > 0 {
                let r = e.range();
                match self
                    .source
                    .get(usize::from(r.start())..usize::from(r.end()))
                {
                    Some(t) => self.found.push(t.to_string()),
                    None => self.fail("a Python statement's text cannot be located".into()),
                }
            }
        }
        fn fail(&mut self, why: String) {
            self.failure.get_or_insert(why);
        }
    }
    let (source, body) = parse(source)?;
    let mut v = Bare {
        source: &source,
        found: Vec::new(),
        failure: None,
    };
    walk::module(&mut v, &body);
    match v.failure {
        Some(why) => Err(why),
        None => Ok(v.found),
    }
}

/// The normalised source and its parsed body, or why it is not read.
fn parse(source: &str) -> Result<(String, rustpython_parser::ast::Suite), String> {
    let source = normalise(source)?;
    let parsed = catch_unwind(AssertUnwindSafe(|| {
        rustpython_parser::ast::Suite::parse(&source, "<string>")
    }));
    match parsed {
        Ok(Ok(body)) => Ok((source, body)),
        Ok(Err(e)) => Err(format!("the Python program does not parse: {e}")),
        Err(_) => Err("the Python parser failed on the program".into()),
    }
}

/// The source as CPython's tokenizer reads it, or why it is not read.
fn normalise(source: &str) -> Result<String, String> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(format!(
            "the Python program is longer than {MAX_SOURCE_BYTES} bytes"
        ));
    }
    if source.contains('\0') {
        return Err("the Python program contains a NUL byte".into());
    }
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let source = source.replace("\r\n", "\n").replace('\r', "\n");
    for line in source.lines().take(2) {
        if let Some(enc) = coding_cookie(line) {
            let enc = enc.to_ascii_lowercase().replace('_', "-");
            if !(enc == "utf-8" || enc == "utf8" || enc.starts_with("utf-8-")) {
                return Err(format!(
                    "the Python program declares the source encoding `{enc}`"
                ));
            }
        }
    }
    Ok(source)
}

/// PEP 263: `^[ \t\f]*#.*?coding[:=][ \t]*([-\w.]+)`.
fn coding_cookie(line: &str) -> Option<&str> {
    let rest = line.trim_start_matches([' ', '\t', '\x0c']);
    let comment = rest.strip_prefix('#')?;
    let mut from = 0;
    while let Some(at) = comment[from..].find("coding") {
        let after = &comment[from + at + "coding".len()..];
        if let Some(value) = after.strip_prefix([':', '=']) {
            let value = value.trim_start_matches([' ', '\t']);
            let end = value
                .find(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_' || c == '.'))
                .unwrap_or(value.len());
            if end > 0 {
                return Some(&value[..end]);
            }
        }
        from += at + "coding".len();
    }
    None
}
