//! Computed write paths the Python-file judge can still prove repo-scoped
//! (user ruling 2026-11-08): a path from iterating a repo-relative directory,
//! a path read from `sys.argv`, and a path held in `self.attr` that a class's
//! constructor received. Kept apart from `python_semantics.rs` for the
//! file-size cap; it extends the same [`Eval`].
//!
//! The ruling: a computed write path is Allow only when it is provably
//! repo-scoped — derived from repo-relative iteration (`glob.glob`,
//! `Path.glob` / `rglob` / `iterdir`, `os.walk`, `os.listdir` rooted at a
//! relative literal such as `crates` or `scripts`, or joined from one), or
//! from a value the script read without changing it. Whenever the path could
//! point at home, `/`, a dotfile or `.git` — an absolute root, `~`,
//! `expanduser`, `Path.home()`, an iteration rooted at `/` or `~`, an unknown
//! origin — it is an Ask. Every reading here answers `None` (no value, so the
//! caller asks) on any shape it does not model.
//!
//! Not modelled, and so NOT refused by this reading (the same as the shell
//! rules for the equivalent literal command): a symlink inside the iterated
//! directory that points elsewhere, and a protected gate/config file nested
//! below the root (`crates/x/.claude/settings.json`) — the root itself is
//! checked ([`under_verdict`]), not every entry an iteration may reach.

use super::{
    analyze_shell_payload, name, shell_quote, top, Ctx, Decision, Effect, Eval, PathVal, Tok, Vals,
    MAX_VALUES,
};

/// The arguments after the script word of every run of the file in the
/// command (one inner list per run), each `Some(word)` when it is a
/// repo-relative literal word ([`argv_word`]) and `None` otherwise.
pub(in crate::detect::interpreter) type Argv = Vec<Vec<Option<String>>>;

/// The placeholder entry name a write below an iterated root is judged with.
const ENTRY: &str = "blastguard-iterated-entry";

/// A `def` parameter: its name and the token span of its default, if any.
type Param = (String, Option<(usize, usize)>);

/// `w` as a repo-relative literal shell word: only plain characters (no
/// quote, `$`, backtick, glob, `~`, backslash or operator), not absolute, not
/// an option, and no `..` component. `None` otherwise.
pub(in crate::detect::interpreter) fn argv_word(w: &str) -> Option<String> {
    let plain = !w.is_empty()
        && w.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_/.,:=@%+-".contains(c));
    let ok = plain && !w.starts_with(['/', '-']) && !w.split('/').any(|c| c == "..");
    ok.then(|| w.to_string())
}

/// `a` as the root of an iteration: `./` prefixes and trailing `/` dropped,
/// the empty path as `.`; `None` when it is absolute, starts with `~`, or has
/// a `..` or dot-named component.
pub(super) fn dir_root(a: &str) -> Option<String> {
    if a.starts_with('/') || a.starts_with('~') || a.contains('\0') {
        return None;
    }
    let mut s = a;
    while let Some(r) = s.strip_prefix("./") {
        s = r;
    }
    let s = s.trim_end_matches('/');
    if s.is_empty() || s == "." {
        return Some(".".to_string());
    }
    if s.starts_with('/')
        || s.split('/')
            .any(|c| c == ".." || (c.starts_with('.') && c != "."))
    {
        return None;
    }
    Some(s.to_string())
}

/// For a relative suffix `b` joined below an iterated root: `None` when it
/// climbs (`..`), else whether it names a dot-named component.
pub(super) fn below(b: &str) -> Option<bool> {
    let comps: Vec<&str> = b.split('/').collect();
    if comps.contains(&"..") {
        return None;
    }
    Some(comps.iter().any(|c| c.starts_with('.') && *c != "."))
}

/// The verdict on removing or writing a path below an iterated root
/// ([`PathVal::Under`]; an [`PathVal::Entry`] used alone is an entry of the
/// current directory). Ask when the iteration lists the top of the working
/// tree with dot-named entries (it can name `.git`, `.githooks`, `.claude`) or
/// the root touches a protected gate/config path; otherwise the shell rules'
/// verdict on the equivalent command — `rm [-rf] <root>/*` for a removal,
/// `: > <root>/<entry>` for a write.
pub(super) fn under_verdict(
    call: &str,
    path: &PathVal,
    effect: Effect,
    depth: usize,
    ctx: &Ctx<'_>,
) -> Decision {
    let (root, hidden) = match path {
        PathVal::Under { root, hidden } => (root.as_str(), *hidden),
        PathVal::Entry => (".", true),
        _ => {
            return Decision::ask(format!(
                "the program calls `{call}` on a path blastguard cannot resolve statically — it \
cannot tell whether that removes or overwrites something irreversible"
            ))
        }
    };
    if root == "." && hidden {
        return Decision::ask(format!(
            "the program calls `{call}` on paths it gets by listing the top of the working tree, \
dot-named entries included — that can name `.git`, `.githooks` or `.claude`, so blastguard cannot \
vouch for it"
        ));
    }
    if crate::exclude::touches_protected(root) {
        return Decision::ask(format!(
            "the program calls `{call}` on paths it gets by iterating `{root}`, which holds \
protected gate/config paths"
        ));
    }
    let dir = shell_quote(root);
    let line = match effect {
        Effect::Remove { recursive: true } => format!("rm -rf {dir}/*"),
        Effect::Remove { recursive: false } => format!("rm {dir}/*"),
        Effect::Write { append: false } => format!(": > {dir}/{ENTRY}"),
        Effect::Write { append: true } => format!(": >> {dir}/{ENTRY}"),
    };
    match analyze_shell_payload(&line, depth, ctx) {
        d @ Decision::Deny(_) => d,
        Decision::Ask(r) => Decision::ask(format!(
            "the program calls `{call}` on paths it gets by iterating `{root}`, which does what \
`{line}` does: {r}"
        )),
        Decision::Allow => Decision::Allow,
    }
}

/// A simple `for` header: `for a[, b, …] in <iterable>:` with plain-name
/// targets. The target names and the iterable's span relative to `st`.
pub(super) fn simple_for_header(st: &[Tok]) -> Option<(Vec<String>, (usize, usize))> {
    if name(st.first()) != Some("for") {
        return None;
    }
    let mut depth = 0usize;
    let mut in_at = None;
    for (k, t) in st.iter().enumerate().skip(1) {
        match t {
            Tok::Punct('(' | '[' | '{') => depth += 1,
            Tok::Punct(')' | ']' | '}') => depth = depth.saturating_sub(1),
            Tok::Name { text, .. } if depth == 0 && text == "in" => {
                in_at = Some(k);
                break;
            }
            _ => {}
        }
    }
    let in_at = in_at?;
    let mut targets = Vec::new();
    for piece in st[1..in_at].split(|t| *t == Tok::Punct(',')) {
        match piece {
            [Tok::Name { text, .. }] if !text.contains('.') && is_ident(text) => {
                targets.push(text.clone())
            }
            _ => return None,
        }
    }
    let mut depth = 0usize;
    for (k, t) in st.iter().enumerate().skip(in_at + 1) {
        match t {
            Tok::Punct('(' | '[' | '{') => depth += 1,
            Tok::Punct(')' | ']' | '}') => depth = depth.saturating_sub(1),
            Tok::Punct(':') if depth == 0 => {
                return (k > in_at + 1).then_some((targets, (in_at + 1, k)));
            }
            _ => {}
        }
    }
    None
}

fn is_ident(s: &str) -> bool {
    s.chars()
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_')
}

/// The indices in `st` of the parameter names of a `def` header.
pub(super) fn def_param_positions(st: &[Tok]) -> Vec<usize> {
    let mut out = Vec::new();
    if name(st.first()) != Some("def") || st.get(2) != Some(&Tok::Punct('(')) {
        return out;
    }
    let mut depth = 0usize;
    for k in 2..st.len() {
        match &st[k] {
            Tok::Punct('(' | '[' | '{') => depth += 1,
            Tok::Punct(')' | ']' | '}') => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    break;
                }
            }
            Tok::Name { .. } if depth == 1 => {
                let prev = matches!(st[k - 1], Tok::Punct('(' | ',' | '*'));
                let next = matches!(st.get(k + 1), Some(Tok::Punct(',' | ')' | '=' | ':')));
                if prev && next {
                    out.push(k);
                }
            }
            _ => {}
        }
    }
    out
}

/// What iterating an expression yields.
enum Elems {
    /// Each element is one of these values.
    Each(Vals),
    /// `os.walk`: `(dirpath, dirnames, filenames)` with dirpath one of these.
    Walk(Vals),
}

/// Names whose presence anywhere lets a program change an attribute, or the
/// object `self` stands for, outside the `self.x = …` statements read here.
const ATTR_POISON: &[&str] = &[
    "setattr",
    "getattr",
    "delattr",
    "vars",
    "globals",
    "locals",
    "type",
    "super",
    "property",
    "namedtuple",
    "dataclasses",
    "dataclass",
    "copy",
    "pickle",
    "cPickle",
    "_pickle",
    "shelve",
    "copyreg",
    "marshal",
    "dill",
    "cloudpickle",
    "jsonpickle",
    "joblib",
    "builtins",
    "__builtins__",
    "__dict__",
    "__setattr__",
    "__getattr__",
    "__getattribute__",
    "__class__",
    "__func__",
    "__self__",
    "__get__",
    "__set__",
    "__new__",
    "__slots__",
    "__init__",
];

impl Eval<'_> {
    /// The values a `for` target takes: target `index` of `count` over the
    /// iterable span `iter`.
    pub(super) fn loop_value(
        &self,
        index: usize,
        count: usize,
        iter: (usize, usize),
    ) -> Option<Vals> {
        match (self.iter_elems(iter.0, iter.1)?, count, index) {
            (Elems::Each(v), 1, _) => Some(v),
            (Elems::Walk(roots), 3, 0) => Some(roots),
            (Elems::Walk(_), 3, _) => Some(vec![PathVal::Entries]),
            _ => None,
        }
    }

    fn iter_elems(&self, s: usize, e: usize) -> Option<Elems> {
        let left = self.budget.get().checked_sub(1)?;
        self.budget.set(left);
        let toks = self.toks;
        if e == s + 1 {
            // A plain name holding `os.walk` name lists.
            let v = self.name_value(name(toks.get(s))?)?;
            return v
                .iter()
                .all(|x| *x == PathVal::Entries)
                .then(|| Elems::Each(vec![PathVal::Entry]));
        }
        if e < s + 3 || toks.get(e - 1) != Some(&Tok::Punct(')')) {
            return None;
        }
        let open = self.open_of(e - 1)?;
        let head = open.checked_sub(1)?;
        let Tok::Name { text: f, .. } = toks.get(head)? else {
            return None;
        };
        if head > s && toks[head - 1] == Tok::Punct('.') {
            // `<chain>.m(…)`: the receiver is everything before the `.`.
            let (recv, end) = self.chain(s, head - 1)?;
            return (end == head - 1)
                .then_some(())
                .and_then(|()| self.path_method_elems(recv, f, open));
        }
        if head != s {
            return None;
        }
        match f.as_str() {
            "sorted" | "list" | "tuple" if self.builtin_genuine(f) => {
                let args = self.args(open);
                let (a, b) = self.arg(open, 0, "")?;
                let positional = args.iter().filter(|x| self.keyword(**x).is_none()).count();
                (positional == 1).then_some(())?;
                self.iter_elems(a, b)
            }
            "glob.glob" | "glob.iglob" if self.module_genuine("glob") => self.glob_elems(open),
            "os.listdir" if self.module_genuine("os") => Some(Elems::Each(vec![PathVal::Entry])),
            "os.walk" if self.module_genuine("os") => self.walk_elems(open),
            m if m.contains('.') && !m.starts_with("os.") && !m.starts_with("glob.") => {
                let (recv, method) = m.rsplit_once('.')?;
                let recv = self.name_value_at(recv, head)?;
                self.path_method_elems(recv, method, open)
            }
            _ => None,
        }
    }

    /// `glob.glob(<literal pattern>[, recursive=…][, include_hidden=…])`.
    fn glob_elems(&self, open: usize) -> Option<Elems> {
        let mut pattern = None;
        let mut hidden = false;
        for a in self.args(open) {
            match self.keyword(a) {
                None if pattern.is_none() => pattern = Some(self.literal(a.0, a.1)?),
                Some("recursive") => {}
                Some("include_hidden") => hidden = true,
                _ => return None,
            }
        }
        let v = glob_value(PathVal::Lit(".".to_string()), &pattern?, hidden)?;
        Some(Elems::Each(vec![v]))
    }

    /// `os.walk(<top>[, topdown=…][, onerror=…])` (no `followlinks`).
    fn walk_elems(&self, open: usize) -> Option<Elems> {
        let args = self.args(open);
        let mut top_span = None;
        for a in args {
            match self.keyword(a) {
                None if top_span.is_none() => top_span = Some(a),
                Some("top") if top_span.is_none() => top_span = Some((a.0 + 2, a.1)),
                Some("topdown" | "onerror") => {}
                _ => return None,
            }
        }
        let (a, b) = top_span?;
        let roots = self
            .expr(a, b)?
            .into_iter()
            .map(|v| match PathVal::Lit(".".to_string()).join(v)? {
                PathVal::Lit(r) => Some(PathVal::Under {
                    root: dir_root(&r)?,
                    hidden: true,
                }),
                PathVal::Under { root, .. } => Some(PathVal::Under { root, hidden: true }),
                _ => None,
            })
            .collect::<Option<Vals>>()?;
        Some(Elems::Walk(roots))
    }

    /// `p.glob(<literal>)`, `p.rglob(<literal>)`, `p.iterdir()` on path values.
    fn path_method_elems(&self, recv: Vals, m: &str, open: usize) -> Option<Elems> {
        let args = self.args(open);
        let pattern = match m {
            "glob" | "rglob" => {
                let [a] = args.as_slice() else {
                    return None;
                };
                if self.keyword(*a).is_some() {
                    return None;
                }
                Some(self.literal(a.0, a.1)?)
            }
            "iterdir" if args.is_empty() => None,
            _ => return None,
        };
        let mut out = Vec::new();
        for r in recv {
            let r = PathVal::Lit(".".to_string()).join(r)?;
            let v = match (m, &pattern) {
                ("glob", Some(p)) => glob_value(r, p, true)?,
                (_, p) => {
                    if let Some(p) = p {
                        if p.starts_with('/') || p.split('/').any(|c| c == "..") {
                            return None;
                        }
                    }
                    match r {
                        PathVal::Lit(a) => PathVal::Under {
                            root: dir_root(&a)?,
                            hidden: true,
                        },
                        PathVal::Under { root, .. } => PathVal::Under { root, hidden: true },
                        _ => return None,
                    }
                }
            };
            out.push(v);
        }
        (out.len() <= MAX_VALUES).then_some(Elems::Each(out))
    }

    /// True when the builtin `n` is not rebound anywhere in the program.
    fn builtin_genuine(&self, n: &str) -> bool {
        !self.rebound.contains(n) && !self.assigns.contains_key(n) && !self.loops.contains_key(n)
    }

    /// True when the module name `m` is bound only by plain `import m` /
    /// `import m.sub` statements (no alias, no `from … import m`, no star
    /// import, no other binding).
    fn module_genuine(&self, m: &str) -> bool {
        if self.assigns.contains_key(m) || self.loops.contains_key(m) || self.nonparam.contains(m) {
            return false;
        }
        for &(s, e) in &self.stmts {
            let st = &self.toks[s..e];
            match name(st.first()) {
                Some("import") => {
                    for item in st[1..].split(|t| *t == Tok::Punct(',')) {
                        let mentions = item
                            .iter()
                            .any(|t| name(Some(t)).is_some_and(|n| top(n) == m));
                        let plain = matches!(item, [Tok::Name { text, .. }] if top(text) == m);
                        if mentions && !plain {
                            return false;
                        }
                    }
                }
                Some("from") => {
                    let Some(ip) = st.iter().position(|t| name(Some(t)) == Some("import")) else {
                        return false;
                    };
                    let rest = &st[ip + 1..];
                    if rest.contains(&Tok::Punct('*'))
                        || rest.iter().any(|t| name(Some(t)) == Some(m))
                    {
                        return false;
                    }
                }
                _ => {}
            }
        }
        true
    }

    /// `sys.argv[N]` (N ≥ 1 a literal) at token `k`: its values over every
    /// run of the file, and the token after the `]`. `None` — no value, so an
    /// Ask — when the runs are not known, a run passes no N-th argument (the
    /// ruling allows only a target the command actually passes; a missing
    /// one is not shown repo-scoped, even though the subscript would raise),
    /// an argument there is not a repo-relative literal word, or the program
    /// may change `sys.argv` ([`Eval::argv_untouched`]).
    pub(super) fn argv_subscript(&self, k: usize) -> Option<(Vals, usize)> {
        let toks = self.toks;
        if name(toks.get(k)) != Some("sys.argv")
            || toks.get(k + 1) != Some(&Tok::Punct('['))
            || toks.get(k + 3) != Some(&Tok::Punct(']'))
        {
            return None;
        }
        let idx: usize = name(toks.get(k + 2))?.parse().ok()?;
        if idx == 0 || !self.argv_untouched() {
            return None;
        }
        let mut out = Vec::new();
        for run in self.argv? {
            out.push(PathVal::Lit(run.get(idx - 1)?.clone()?));
        }
        (out.len() <= MAX_VALUES).then_some((out, k + 4))
    }

    /// True when the program only ever reads `sys.argv` as `sys.argv[N]`
    /// outside any assignment target, and nothing else can reach it: `sys` is
    /// named only in `import` statements, no other name has an `argv`
    /// component, and no reflective helper ([`ATTR_POISON`]-style) appears.
    fn argv_untouched(&self) -> bool {
        let toks = self.toks;
        for (si, &(s, e)) in self.stmts.iter().enumerate() {
            let first = name(toks.get(s));
            for j in s..e {
                let Some(n) = name(toks.get(j)) else {
                    continue;
                };
                if n == "sys" {
                    if first != Some("import") {
                        return false;
                    }
                    continue;
                }
                if n == "sys.argv" {
                    let indexed = toks.get(j + 1) == Some(&Tok::Punct('['))
                        && name(toks.get(j + 2))
                            .is_some_and(|d| d.chars().all(|c| c.is_ascii_digit()))
                        && toks.get(j + 3) == Some(&Tok::Punct(']'));
                    if !indexed || self.is_target(si, j) || self.is_target(si, j + 3) {
                        return false;
                    }
                    continue;
                }
                let comps: Vec<&str> = n.split('.').collect();
                if comps.iter().any(|c| {
                    matches!(
                        *c,
                        "argv"
                            | "orig_argv"
                            | "modules"
                            | "setattr"
                            | "delattr"
                            | "vars"
                            | "globals"
                            | "locals"
                            | "__dict__"
                            | "builtins"
                            | "__builtins__"
                    )
                }) {
                    return false;
                }
            }
        }
        true
    }

    /// The values of `self.<x>` read at token `k`, resolved through its
    /// class: the read must sit in a method (first parameter `self`) of a
    /// module-level class `C` with no base other than `object`, created only
    /// by `C(…)` calls; every write of an attribute named `x` anywhere in the
    /// program must be a `self.x = <rhs>` statement directly in some such
    /// class's `__init__`; `<rhs>` is a parameter of that `__init__` (its
    /// values are the matching arguments, or the default, of every
    /// constructor call) or a statically readable expression. `None` on any
    /// other shape, or when the program uses a reflective helper
    /// ([`ATTR_POISON`]), a decorator, a string naming `x`, or binds `x` or
    /// `self` some other way.
    pub(super) fn attr_value(&self, x: &str, k: usize) -> Option<Vals> {
        let left = self.budget.get().checked_sub(1)?;
        self.budget.set(left);
        if !self.attrs_closed(x) {
            return None;
        }
        let method = self.enclosing_scope(self.stmt_index(k)?)?;
        let params = self.def_params(method)?;
        if params.first().map(|p| p.0.as_str()) != Some("self") {
            return None;
        }
        let cls = self.plain_class(self.enclosing_scope(method)?)?;
        self.class_calls(&cls)?;
        let mut out = Vec::new();
        for (si, &(s, e)) in self.stmts.iter().enumerate() {
            for j in s..e {
                if self.names_attr(j, x) && self.is_target(si, j) {
                    out.extend(self.approved_write(si, x)?);
                    if out.len() > MAX_VALUES {
                        return None;
                    }
                }
            }
        }
        Some(out)
    }

    /// No reflective helper, decorator, string naming `x`, or other binding
    /// of `x` / `self` in the program.
    fn attrs_closed(&self, x: &str) -> bool {
        let toks = self.toks;
        for &n in &[x, "self"] {
            if self.assigns.contains_key(n)
                || self.loops.contains_key(n)
                || self.nonparam.contains(n)
                || self.imported.contains(n)
            {
                return false;
            }
        }
        for &(s, e) in &self.stmts {
            if toks[s] == Tok::Punct('@') {
                return false;
            }
            for j in s..e {
                match &toks[j] {
                    Tok::Name { text, .. } => {
                        let def_header = j == s + 1 && name(toks.get(s)) == Some("def");
                        if text.split('.').any(|c| ATTR_POISON.contains(&c))
                            && !(def_header && text == "__init__")
                        {
                            return false;
                        }
                    }
                    Tok::Str { text, .. } if names_word(text, x) => return false,
                    _ => {}
                }
            }
        }
        true
    }

    /// True when token `j` names an attribute `x` (`a.x`, or `.x` after a
    /// call or subscript).
    fn names_attr(&self, j: usize, x: &str) -> bool {
        match name(self.toks.get(j)) {
            Some(n) if n == x => j > 0 && self.toks[j - 1] == Tok::Punct('.'),
            Some(n) => n.rsplit_once('.').is_some_and(|(_, last)| last == x),
            None => false,
        }
    }

    /// True when token `j` of statement `si` may be (part of) a binding
    /// target: before a top-level `=`, before an augmented operator, after
    /// `as`, between a `for` and its `in`, or in a `del` / `global` /
    /// `nonlocal` statement.
    fn is_target(&self, si: usize, j: usize) -> bool {
        let toks = self.toks;
        let (s, e) = self.stmts[si];
        if matches!(name(toks.get(s)), Some("del" | "global" | "nonlocal")) {
            return true;
        }
        if j > s && name(toks.get(j - 1)) == Some("as") {
            return true;
        }
        let op = |t: Option<&Tok>| matches!(t, Some(Tok::Punct(c)) if "+-*/%&|^@<>".contains(*c));
        if op(toks.get(j + 1))
            && (toks.get(j + 2) == Some(&Tok::Punct('='))
                || (op(toks.get(j + 2)) && toks.get(j + 3) == Some(&Tok::Punct('='))))
        {
            return true;
        }
        let st = &toks[s..e];
        let mut depth = 0usize;
        let mut open_for = false;
        for (r, t) in st.iter().enumerate() {
            let abs = s + r;
            if abs > j && super::is_assign_eq(st, r, depth) {
                return true;
            }
            if abs < j {
                match name(Some(t)) {
                    Some("for") => open_for = true,
                    Some("in") => open_for = false,
                    _ => {}
                }
            }
            match t {
                Tok::Punct('(' | '[' | '{') => depth += 1,
                Tok::Punct(')' | ']' | '}') => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
        open_for
    }

    /// The values a `self.x = <rhs>` statement `si` stores, when it is
    /// directly in the `__init__` of a [`Eval::plain_class`].
    fn approved_write(&self, si: usize, x: &str) -> Option<Vals> {
        let toks = self.toks;
        let (s, e) = self.stmts[si];
        let st = &toks[s..e];
        let lhs_ok = name(st.first()) == Some(format!("self.{x}").as_str());
        let mut depth = 0usize;
        let mut eqs = 0usize;
        for (r, t) in st.iter().enumerate() {
            if super::is_assign_eq(st, r, depth) {
                eqs += 1;
            }
            match t {
                Tok::Punct('(' | '[' | '{') => depth += 1,
                Tok::Punct(')' | ']' | '}') => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
        if !lhs_ok || eqs != 1 || st.get(1) != Some(&Tok::Punct('=')) || st.len() < 3 {
            return None;
        }
        let init = self.enclosing_scope(si)?;
        if name(toks.get(self.stmts[init].0 + 1)) != Some("__init__") {
            return None;
        }
        let params = self.def_params(init)?;
        if params.first().map(|p| p.0.as_str()) != Some("self") {
            return None;
        }
        let cls = self.plain_class(self.enclosing_scope(init)?)?;
        if let [_, _, Tok::Name { text, .. }] = st {
            if let Some(pi) = params.iter().position(|p| p.0 == *text) {
                return (pi > 0)
                    .then_some(())
                    .and_then(|()| self.param_values(&cls, &params, pi));
            }
        }
        self.expr(s + 2, e)
    }

    /// The values parameter `pi` of class `cls`'s `__init__` receives over
    /// every `cls(…)` call: the matching keyword or positional argument, else
    /// the default; a call without either raises and contributes nothing.
    fn param_values(&self, cls: &str, params: &[Param], pi: usize) -> Option<Vals> {
        let pname = &params[pi].0;
        let mut out = Vec::new();
        for open in self.class_calls(cls)? {
            let mut pos = 0usize;
            let mut chosen = None;
            for a in self.args(open) {
                if self.toks.get(a.0) == Some(&Tok::Punct('*')) {
                    return None;
                }
                match self.keyword(a) {
                    Some(kw) => {
                        if !params.iter().skip(1).any(|p| p.0 == kw) {
                            return None;
                        }
                        if kw == pname {
                            chosen = Some((a.0 + 2, a.1));
                        }
                    }
                    None => {
                        pos += 1;
                        if pos == pi {
                            chosen = Some(a);
                        }
                    }
                }
            }
            if let Some((a, b)) = chosen.or(params[pi].1) {
                out.extend(self.expr(a, b)?);
                if out.len() > MAX_VALUES {
                    return None;
                }
            }
        }
        Some(out)
    }

    /// The `(` of every `cls(…)` call, when `cls` has exactly one header and
    /// is otherwise named only as the callee of such calls.
    fn class_calls(&self, cls: &str) -> Option<Vec<usize>> {
        let toks = self.toks;
        let mut headers = 0usize;
        let mut opens = Vec::new();
        for &(s, e) in &self.stmts {
            let first = name(toks.get(s));
            for j in s..e {
                let Some(n) = name(toks.get(j)) else {
                    continue;
                };
                if top(n) != cls {
                    continue;
                }
                if n != cls {
                    return None;
                }
                if j == s + 1 && first == Some("class") {
                    headers += 1;
                    continue;
                }
                if j > s && toks[j - 1] == Tok::Punct('.') {
                    continue;
                }
                let callee = toks.get(j + 1) == Some(&Tok::Punct('('));
                let binding = matches!(
                    first,
                    Some("def" | "class" | "import" | "from" | "global" | "nonlocal" | "del")
                );
                if !callee || binding {
                    return None;
                }
                opens.push(j + 1);
            }
        }
        (headers == 1).then_some(opens)
    }

    /// The name of the class statement `si` when it is a module-level
    /// `class C:` / `class C():` / `class C(object):` header.
    fn plain_class(&self, si: usize) -> Option<String> {
        if self.indent(si)? != 0 {
            return None;
        }
        let (s, e) = self.stmts[si];
        let st = &self.toks[s..e];
        let cls = match st {
            [Tok::Name { text: kw, .. }, Tok::Name { text: c, .. }, rest @ ..] if kw == "class" => {
                let ok = matches!(
                    rest,
                    [Tok::Punct(':')] | [Tok::Punct('('), Tok::Punct(')'), Tok::Punct(':')]
                ) || matches!(rest, [Tok::Punct('('), Tok::Name { text: o, .. }, Tok::Punct(')'), Tok::Punct(':')] if o == "object");
                ok.then(|| c.clone())?
            }
            _ => return None,
        };
        (!cls.contains('.')).then_some(cls)
    }

    /// The parameters of the `def` statement `si`: name and default span,
    /// `None` for any other shape (`*`, `/`, annotations).
    fn def_params(&self, si: usize) -> Option<Vec<Param>> {
        let toks = self.toks;
        let (s, e) = self.stmts[si];
        if name(toks.get(s)) != Some("def") || toks.get(s + 2) != Some(&Tok::Punct('(')) {
            return None;
        }
        if self.close(s + 2) >= e {
            return None;
        }
        let mut out = Vec::new();
        for (a, b) in self.args(s + 2) {
            match &toks[a..b] {
                [Tok::Name { text, .. }] if !text.contains('.') => out.push((text.clone(), None)),
                [Tok::Name { text, .. }, Tok::Punct('='), ..]
                    if !text.contains('.') && b > a + 2 =>
                {
                    out.push((text.clone(), Some((a + 2, b))))
                }
                _ => return None,
            }
        }
        Some(out)
    }

    fn stmt_index(&self, k: usize) -> Option<usize> {
        self.stmts.iter().position(|&(s, e)| s <= k && k < e)
    }

    /// The indentation of statement `si`, when it starts its own physical
    /// line with a name (not after `;` or a backslash continuation).
    fn indent(&self, si: usize) -> Option<usize> {
        let Tok::Name { start, .. } = self.toks.get(self.stmts.get(si)?.0)? else {
            return None;
        };
        let line_start = self.src[..*start].rfind('\n').map_or(0, |p| p + 1);
        if line_start > 0 && self.src[..line_start - 1].ends_with('\\') {
            return None;
        }
        let lead = &self.src[line_start..*start];
        lead.chars()
            .all(|c| c == ' ' || c == '\t')
            .then(|| lead.chars().count())
    }

    /// The `def` or `class` statement whose body holds statement `si`
    /// (through `if` / `for` / `while` / `with` / `try` blocks), `None` at
    /// module level or when the nesting cannot be read.
    fn enclosing_scope(&self, si: usize) -> Option<usize> {
        let mut cur = si;
        loop {
            let ind = self.indent(cur)?;
            let j = (0..cur)
                .rev()
                .find(|&j| self.indent(j).is_some_and(|i| i < ind))?;
            match name(self.toks.get(self.stmts[j].0)) {
                Some("def" | "class") => return Some(j),
                Some(
                    "if" | "elif" | "else" | "for" | "while" | "with" | "try" | "except"
                    | "finally",
                ) => cur = j,
                _ => return None,
            }
        }
    }
}

/// `pattern` (relative, no `..`) globbed from `base`: the literal path when it
/// has no wildcard, else the iteration below its literal prefix.
fn glob_value(base: PathVal, pattern: &str, hidden: bool) -> Option<PathVal> {
    if pattern.starts_with('/') || pattern.starts_with('~') {
        return None;
    }
    let comps: Vec<&str> = pattern.split('/').collect();
    if comps.contains(&"..") {
        return None;
    }
    let Some(first) = comps.iter().position(|c| c.contains(['*', '?', '['])) else {
        return base.join(PathVal::Lit(pattern.to_string()));
    };
    let below_hidden = comps[first..].iter().any(|c| c.starts_with('.'));
    let hidden = hidden || below_hidden;
    match base.join(PathVal::Lit(comps[..first].join("/")))? {
        PathVal::Lit(r) => Some(PathVal::Under {
            root: dir_root(&r)?,
            hidden,
        }),
        PathVal::Under { root, hidden: h } => Some(PathVal::Under {
            root,
            hidden: h || hidden,
        }),
        _ => None,
    }
}

/// True when `text` holds `word` delimited by non-identifier characters.
fn names_word(text: &str, word: &str) -> bool {
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    text.match_indices(word).any(|(i, _)| {
        let before = text[..i].chars().next_back();
        let after = text[i + word.len()..].chars().next();
        !before.is_some_and(ident) && !after.is_some_and(ident)
    })
}
