//! Static values: the string a Python expression evaluates to when the
//! program text alone determines it, else `None` (never a guess).
//!
//! Determined: a string / bytes literal (escapes decoded by the parser), an
//! f-string without a replacement field, `+` of two determined strings, a
//! name bound exactly once by a module-level `name = value`
//! ([`super::bind::Resolved::Value`]), `sys.argv[i]` of a literal command-line
//! word, `str(x)`, `os.fspath(x)`, `os.path.join(…)`, `pathlib.Path(…)` (and
//! `PurePath` / `PosixPath` / `PurePosixPath`) and `/` on such a path.
//! Anything else — a call, a subscript, a replacement field, a name bound
//! twice or inside a function — is `None`.

use rustpython_parser::ast::{self, Constant, Expr, Operator, UnaryOp};

use super::bind::{Bindings, Resolved};

/// Recursion cap of a resolution chain (name → value → name …).
const MAX_CHAIN: usize = 16;

pub(super) struct Cx<'a> {
    pub b: &'a Bindings,
    /// `sys.argv`; `None` for a word whose value is not determined.
    pub argv: &'a [Option<String>],
    /// The (normalised) source the AST ranges index.
    pub src: &'a str,
}

const PATH_TYPES: &[&str] = &[
    "pathlib.Path",
    "pathlib.PurePath",
    "pathlib.PosixPath",
    "pathlib.PurePosixPath",
];

impl Cx<'_> {
    /// The qualified name an attribute chain stands for when its head is a
    /// name bound only by imports (`os.path.join` → `os.path.join`,
    /// `from os import path as p; p.join` → `os.path.join`).
    pub(super) fn qual(&self, e: &Expr) -> Option<String> {
        match e {
            Expr::Name(n) => match self.b.resolve(n.id.as_str()) {
                Resolved::Module(q) => Some(q.to_string()),
                Resolved::Unbound | Resolved::Value(_) | Resolved::Ambiguous { .. } => None,
            },
            Expr::Attribute(a) => Some(format!("{}.{}", self.qual(&a.value)?, a.attr.as_str())),
            _ => None,
        }
    }

    /// Whether `e` is the builtin `name` (unbound in the program).
    pub(super) fn is_builtin(&self, e: &Expr, name: &str) -> bool {
        matches!(e, Expr::Name(n) if n.id.as_str() == name
            && matches!(self.b.resolve(name), Resolved::Unbound))
    }

    pub(super) fn string(&self, e: &Expr) -> Option<String> {
        self.string_at(e, 0)
    }

    pub(super) fn path(&self, e: &Expr) -> Option<String> {
        self.path_at(e, 0)
    }

    /// A list / tuple literal of determined strings (no `*x`).
    pub(super) fn strings(&self, e: &Expr) -> Option<Vec<String>> {
        let elts = match e {
            Expr::List(l) => &l.elts,
            Expr::Tuple(t) => &t.elts,
            Expr::Name(n) => match self.b.resolve(n.id.as_str()) {
                Resolved::Value(v) => return self.strings(v),
                _ => return None,
            },
            _ => return None,
        };
        elts.iter()
            .map(|x| match x {
                Expr::Starred(_) => None,
                x => self.path(x),
            })
            .collect()
    }

    fn string_at(&self, e: &Expr, depth: usize) -> Option<String> {
        if depth > MAX_CHAIN {
            return None;
        }
        match e {
            Expr::Constant(c) => self.literal(c),
            Expr::JoinedStr(j) => j
                .values
                .iter()
                .map(|v| match v {
                    Expr::Constant(c) => self.literal(c),
                    _ => None,
                })
                .collect(),
            Expr::BinOp(b) if matches!(b.op, Operator::Add) => Some(format!(
                "{}{}",
                self.string_at(&b.left, depth + 1)?,
                self.string_at(&b.right, depth + 1)?
            )),
            Expr::Name(n) => match self.b.resolve(n.id.as_str()) {
                Resolved::Value(v) => self.path_at(v, depth + 1),
                _ => None,
            },
            Expr::Subscript(s) => {
                if self.qual(&s.value).as_deref() != Some("sys.argv") {
                    return None;
                }
                let i = int(&s.slice)?;
                let len = i64::try_from(self.argv.len()).ok()?;
                let at = if i < 0 { len + i } else { i };
                let at = usize::try_from(at).ok()?;
                self.argv.get(at)?.clone()
            }
            Expr::Call(c) => self.call_string(c, depth),
            _ => None,
        }
    }

    /// A string / bytes literal's value.
    fn literal(&self, c: &ast::ExprConstant) -> Option<String> {
        constant_str(&c.value)
    }

    /// Whether a `\N{…}` named escape appears anywhere in the program text.
    pub(super) fn has_named_escape(&self) -> bool {
        self.src.contains("\\N{")
    }

    fn call_string(&self, c: &ast::ExprCall, depth: usize) -> Option<String> {
        if !c.keywords.is_empty() {
            return None;
        }
        if self.is_builtin(&c.func, "str") {
            let [x] = c.args.as_slice() else { return None };
            return self.path_at(x, depth + 1);
        }
        match self.qual(&c.func)?.as_str() {
            "os.fspath" => {
                let [x] = c.args.as_slice() else { return None };
                self.path_at(x, depth + 1)
            }
            "os.path.join" | "posixpath.join" => {
                let parts: Option<Vec<String>> =
                    c.args.iter().map(|a| self.path_at(a, depth + 1)).collect();
                let parts = parts?;
                let (first, rest) = parts.split_first()?;
                Some(
                    rest.iter()
                        .fold(first.clone(), |acc, p| os_path_join(&acc, p)),
                )
            }
            q if PATH_TYPES.contains(&q) => self.path_at(&Expr::Call(c.clone()), depth + 1),
            _ => None,
        }
    }

    fn path_at(&self, e: &Expr, depth: usize) -> Option<String> {
        if depth > MAX_CHAIN {
            return None;
        }
        match e {
            Expr::Call(c)
                if self
                    .qual(&c.func)
                    .is_some_and(|q| PATH_TYPES.contains(&q.as_str())) =>
            {
                if !c.keywords.is_empty() {
                    return None;
                }
                let parts: Option<Vec<String>> =
                    c.args.iter().map(|a| self.path_at(a, depth + 1)).collect();
                let joined = parts?
                    .iter()
                    .fold(String::new(), |acc, p| os_path_join(&acc, p));
                Some(pathlib_normal(&joined))
            }
            Expr::BinOp(b) if matches!(b.op, Operator::Div) => {
                let left = self.path_at(&b.left, depth + 1)?;
                let right = self.path_at(&b.right, depth + 1)?;
                Some(pathlib_normal(&os_path_join(&left, &right)))
            }
            Expr::Name(n) => match self.b.resolve(n.id.as_str()) {
                Resolved::Value(v) => self.path_at(v, depth + 1),
                _ => None,
            },
            other => self.string_at(other, depth + 1),
        }
    }
}

/// An integer literal (`3`, `-1`).
pub(super) fn int(e: &Expr) -> Option<i64> {
    match e {
        Expr::Constant(c) => match &c.value {
            Constant::Int(i) => i64::try_from(i).ok(),
            Constant::Bool(b) => Some(i64::from(*b)),
            _ => None,
        },
        Expr::UnaryOp(u) if matches!(u.op, UnaryOp::USub) => int(&u.operand).map(|v| -v),
        _ => None,
    }
}

/// A `True` / `False` literal.
pub(super) fn bool_lit(e: &Expr) -> Option<bool> {
    match e {
        Expr::Constant(c) => match &c.value {
            Constant::Bool(b) => Some(*b),
            _ => None,
        },
        _ => None,
    }
}

fn constant_str(c: &Constant) -> Option<String> {
    match c {
        Constant::Str(s) => Some(s.clone()),
        Constant::Bytes(b) => String::from_utf8(b.clone()).ok(),
        _ => None,
    }
}

/// `os.path.join(a, b)` on POSIX: an absolute `b` replaces `a`.
fn os_path_join(a: &str, b: &str) -> String {
    if b.starts_with('/') || a.is_empty() {
        return b.to_string();
    }
    if a.ends_with('/') {
        format!("{a}{b}")
    } else {
        format!("{a}/{b}")
    }
}

/// The string `pathlib` gives a path: empty and `.` components and repeated
/// slashes collapsed (`..` kept), `.` for an empty path.
fn pathlib_normal(p: &str) -> String {
    let lead = if p.starts_with("//") && !p.starts_with("///") {
        "//"
    } else if p.starts_with('/') {
        "/"
    } else {
        ""
    };
    let parts: Vec<&str> = p
        .split('/')
        .filter(|c| !c.is_empty() && *c != ".")
        .collect();
    let body = parts.join("/");
    if lead.is_empty() && body.is_empty() {
        ".".to_string()
    } else {
        format!("{lead}{body}")
    }
}
