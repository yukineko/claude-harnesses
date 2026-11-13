//! Write/move-capable callables reached other than as the callee of a call
//! the path reading analyses directly (backlog c8bf27ea). Kept apart from
//! `python_semantics.rs` for the file-size cap; it extends the same
//! [`Eval`].
//!
//! The path reading ([`super::path_calls_verdict`]) judges `X.m(args)` and
//! `os.f(args)` by their resolved path. The same callable can be invoked
//! without that call shape, and each such shape used to be Allow whatever
//! path it touched:
//!
//! * `Path('/etc/hosts').write_text.__call__('x')` — read as the call
//!   `Path('/etc/hosts').write_text('x')` (every trailing `.__call__` of the
//!   callee is dropped, [`callees`]);
//! * the unbound `Path.replace(Path('/tmp/z'), '/etc/hosts')` — read as
//!   `Path('/tmp/z').replace('/etc/hosts')`: the first argument (or `self=`)
//!   is the receiver and the remaining arguments are those of the bound call
//!   ([`Call`]);
//! * `w = Path('/etc/hosts').write_text; w('x')` — when EVERY binding of `w`
//!   is a simple `w = <reference>` and `w` is read nowhere except as a direct
//!   call, the call `w(…)` is read as the referenced call (one per binding).
//!
//! STRUCTURAL FAIL-CLOSED: any other reference to such a callable — stored in
//! a name that is also used another way, passed as an argument
//! (`functools.partial(p.write_text, 'x')`), put in a container, followed by
//! another attribute (`.__get__`), named by a literal to `getattr` /
//! `operator.methodcaller` / `operator.attrgetter` — is judged here by what
//! it could do to its receiver ([`escaped_reference_verdict`]): a resolved
//! receiver is judged as the same shell command as the direct call (a
//! `chmod` as mode `000`, an `open` as a truncating write); an unresolved
//! receiver, an unbound class reference, a module function such as
//! `os.remove`, and the destination of a `replace` / `rename` are an Ask
//! (Deny where no human can answer), never an Allow.
//!
//! Not refused (Allow as before): a reference whose receiver is a string
//! literal (`'a'.replace`) or the unrebound builtin `str` / `bytes` /
//! `bytearray` (`str.replace`), and an attribute of an imported module other
//! than `pathlib` / `os` / `posix` / `shutil` / `io` / `codecs` / `builtins`
//! (`webbrowser.open`) — none of these is a filesystem path method. NOT
//! MODELLED (so not refused): `getattr` with a non-literal attribute name, and
//! a callable reached through anything other than a name of the program text
//! (`vars(Path)['write_text']`, `Path.__dict__`).

use super::{judge_paths, name, top, Ctx, Decision, Effect, Eval, Tok, Vals};

/// pathlib methods that write, move, re-permission or remove their receiver.
const PATH_METHODS: &[&str] = &[
    "write_text",
    "write_bytes",
    "touch",
    "symlink_to",
    "hardlink_to",
    "chmod",
    "lchmod",
    "unlink",
    "rmdir",
    "replace",
    "rename",
    "open",
];

/// Module-level callables the path reading judges by their path arguments
/// (canonical spellings, after [`super::rewrite`]).
const PATH_FUNCTIONS: &[&str] = &[
    "shutil.rmtree",
    "os.remove",
    "os.unlink",
    "os.rmdir",
    "os.removedirs",
    "shutil.copy",
    "shutil.copy2",
    "shutil.copyfile",
    "shutil.copytree",
    "shutil.move",
    "os.rename",
    "os.replace",
    "os.renames",
    "os.truncate",
    "os.chmod",
    "os.lchmod",
    "os.chown",
    "os.lchown",
    "shutil.chown",
    "open",
    "io.open",
    "codecs.open",
    "builtins.open",
    "os.open",
];

/// The pathlib classes whose methods are [`PATH_METHODS`]; `Cls.m` is the
/// unbound method.
const PATH_CLASSES: &[&str] = &["pathlib.Path", "pathlib.PosixPath", "pathlib.WindowsPath"];

/// Imported modules whose attributes may be one of the callables above.
const PATH_MODULES: &[&str] = &[
    "pathlib", "os", "posix", "shutil", "io", "codecs", "builtins",
];

/// `n` with every trailing `.__call__` removed.
fn strip_call(n: &str) -> &str {
    let mut n = n;
    while let Some(s) = n.strip_suffix(".__call__") {
        n = s;
    }
    n
}

/// True when `n` (a dotted name, `.__call__` stripped) names a pathlib
/// class's method: `pathlib.Path.write_text`.
fn unbound_method(n: &str) -> bool {
    n.rsplit_once('.')
        .is_some_and(|(cls, m)| PATH_CLASSES.contains(&cls) && PATH_METHODS.contains(&m))
}

/// One call as the path reading judges it.
pub(super) struct Call<'t> {
    /// The callee: a canonical dotted name (`os.remove`, `p.write_text`) or,
    /// when `attr`, the method name after a `.`.
    pub(super) n: &'t str,
    /// The callee is an attribute of the expression before token `at`.
    pub(super) attr: bool,
    /// The token the receiver is read from ([`Eval::receiver`]).
    at: usize,
    /// The `(` of the call's argument list.
    open: usize,
    /// The receiver is the first argument (or `self=`): an unbound call.
    unbound: bool,
}

impl Call<'_> {
    /// The argument span that is the receiver of an unbound call.
    fn receiver_span(&self, ev: &Eval<'_>) -> Option<(usize, usize)> {
        let args = ev.args(self.open);
        if let Some(a) = args.iter().find(|a| ev.keyword(**a) == Some("self")) {
            return Some(*a);
        }
        args.into_iter().find(|a| ev.keyword(*a).is_none())
    }

    /// The argument spans of the call, the receiver of an unbound call left out.
    pub(super) fn args(&self, ev: &Eval<'_>) -> Vec<(usize, usize)> {
        let all = ev.args(self.open);
        if !self.unbound {
            return all;
        }
        let recv = self.receiver_span(ev);
        all.into_iter().filter(|a| Some(*a) != recv).collect()
    }

    /// The positional argument `idx` of the call, or its keyword `kw`
    /// ([`Eval::arg`]), counted after the receiver of an unbound call.
    pub(super) fn arg(&self, ev: &Eval<'_>, idx: usize, kw: &str) -> Option<(usize, usize)> {
        let args = self.args(ev);
        if let Some(a) = args.iter().find(|a| ev.keyword(**a) == Some(kw)) {
            return Some((a.0 + 2, a.1));
        }
        args.into_iter()
            .filter(|a| ev.keyword(*a).is_none())
            .nth(idx)
    }

    /// The paths the receiver of a method call may name, `None` when unknown.
    pub(super) fn receiver(&self, ev: &Eval<'_>) -> Option<Vals> {
        if !self.unbound {
            return ev.receiver(self.at, self.n);
        }
        let (s, e) = self.receiver_span(ev)?;
        let s = if ev.keyword((s, e)).is_some() {
            s + 2
        } else {
            s
        };
        ev.expr(s, e)
    }
}

/// The calls that the call whose callee token is `k` (text `text`, an
/// attribute when `attr`) performs, as the path reading judges them: the
/// callee with `.__call__` dropped, the unbound pathlib form with its
/// receiver argument, and a name bound only to references read as each
/// referenced call ([`alias_bindings`]).
pub(super) fn callees<'t>(ev: &Eval<'t>, k: usize, text: &'t str, attr: bool) -> Vec<Call<'t>> {
    let n = strip_call(text);
    let direct = |n: &'t str, attr: bool, at: usize| Call {
        n,
        attr,
        at,
        open: k + 1,
        unbound: !attr && unbound_method(n),
    };
    if !attr && !n.contains('.') {
        if let Some(binds) = alias_bindings(ev, n) {
            return binds
                .into_iter()
                .map(|(r, rn, rattr)| direct(rn, rattr, r))
                .collect();
        }
    }
    vec![direct(n, attr, k)]
}

/// True when the reference `rn` (a token text with `.__call__` stripped; an
/// attribute when `attr`) names a callable the path reading judges.
fn path_capable(rn: &str, attr: bool) -> bool {
    let last = rn.rsplit('.').next().unwrap_or(rn);
    if attr {
        return PATH_METHODS.contains(&last);
    }
    PATH_FUNCTIONS.contains(&rn) || (rn.contains('.') && PATH_METHODS.contains(&last))
}

/// The references the plain name `w` is bound to — `(token, text, attr)` per
/// binding — when every binding of `w` is a simple `w = <reference>` to a
/// write/move-capable callable: a dotted name (`os.remove`, `p.write_text`,
/// `open`) or a postfix chain ending in `.m` (`Path('/x').write_text`).
/// `None` when `w` is rebound, a loop target, unassigned, or has any other
/// binding.
fn alias_bindings<'t>(ev: &Eval<'t>, w: &str) -> Option<Vec<(usize, &'t str, bool)>> {
    if ev.rebound.contains(w) || ev.loops.contains_key(w) {
        return None;
    }
    let spans = ev.assigns.get(w)?;
    let mut out = Vec::new();
    for &(s, e) in spans {
        let r = e.checked_sub(1)?;
        if r < s {
            return None;
        }
        let Some(Tok::Name { text, .. }) = ev.toks.get(r) else {
            return None;
        };
        let rn = strip_call(text);
        let attr = r > s && ev.toks[r - 1] == Tok::Punct('.');
        let whole = if attr {
            ev.chain_start(r - 1) == Some(s)
        } else {
            r == s
        };
        if !whole || !path_capable(rn, attr) {
            return None;
        }
        out.push((r, rn, attr));
    }
    (!out.is_empty()).then_some(out)
}

/// True when the name `w` is resolved as an alias ([`alias_bindings`]) and
/// every occurrence of it is either the target of one of its bindings or the
/// callee of a direct call (`w(…)`, `w.__call__(…)`) — so every use of the
/// referenced callable is judged at its call.
fn alias_fully_called(ev: &Eval<'_>, w: &str) -> bool {
    if alias_bindings(ev, w).is_none() {
        return false;
    }
    let targets: Vec<usize> = ev
        .assigns
        .get(w)
        .map(|v| v.iter().filter_map(|(s, _)| s.checked_sub(2)).collect())
        .unwrap_or_default();
    let toks = ev.toks;
    toks.iter().enumerate().all(|(j, t)| {
        let Tok::Name { text, .. } = t else {
            return true;
        };
        if j > 0 && toks[j - 1] == Tok::Punct('.') {
            return true;
        }
        if text != w && !text.strip_prefix(w).is_some_and(|x| x.starts_with('.')) {
            return true;
        }
        let target = targets.contains(&j) && text == w;
        let called = strip_call(text) == w && toks.get(j + 1) == Some(&Tok::Punct('('));
        target || called
    })
}

/// What a reference to the method `m` could do to its receiver, as the path
/// reading's effect (the restrictive reading where the call's arguments are
/// unknown: a `chmod` as mode `000`, an `open` as a truncating write).
fn method_effect(m: &str) -> Option<Effect> {
    Some(match m {
        "write_text" | "write_bytes" | "symlink_to" | "hardlink_to" | "open" => {
            Effect::Write { append: false }
        }
        "touch" => Effect::Write { append: true },
        "unlink" | "rmdir" | "replace" | "rename" => Effect::Remove { recursive: false },
        "chmod" | "lchmod" => Effect::Chmod { mode: None },
        _ => return None,
    })
}

/// The reason for a reference whose target cannot be known at all.
fn escaped(what: &str) -> Decision {
    Decision::ask(format!(
        "the program uses `{what}` other than by calling it directly (it is stored, passed or \
looked up) — blastguard cannot see which path it is applied to, so it cannot tell whether that \
removes, overwrites or re-permissions something irreversible"
    ))
}

/// Judge a reference to the method `m` whose receiver may name `recv`
/// (`None`: unresolved). `shown` names the reference in the reason.
fn judge_method_ref(
    shown: &str,
    m: &str,
    recv: Option<Vals>,
    depth: usize,
    ctx: &Ctx<'_>,
    pending: &mut Option<Decision>,
) -> Option<Decision> {
    let effect = method_effect(m)?;
    let resolved = recv.is_some();
    if let Some(d) = judge_paths(shown, effect, recv, depth, ctx, pending) {
        return Some(d);
    }
    if resolved && matches!(m, "replace" | "rename") {
        // The destination is an argument of a call blastguard does not see.
        pending.get_or_insert(escaped(shown));
    }
    None
}

/// True when `name` is an unrebound, unassigned builtin string type.
fn string_type(ev: &Eval<'_>, n: &str) -> bool {
    matches!(n, "str" | "bytes" | "bytearray")
        && !ev.rebound.contains(n)
        && !ev.assigns.contains_key(n)
        && !ev.loops.contains_key(n)
}

/// A reference found in a name token: a module function, or a method.
enum Ref {
    Function,
    Method,
}

/// The write/move-capable callable the name token `text` refers to, with
/// the component index where it ends.
fn find_ref(text: &str, attr: bool) -> Option<(Ref, usize)> {
    let comps: Vec<&str> = text.split('.').collect();
    for i in 0..comps.len() {
        if !attr && PATH_FUNCTIONS.contains(&comps[..=i].join(".").as_str()) {
            return Some((Ref::Function, i));
        }
        if (i >= 1 || attr) && PATH_METHODS.contains(&comps[i]) {
            return Some((Ref::Method, i));
        }
    }
    None
}

/// The verdict on every write/move-capable callable of the program that is
/// referenced other than as the callee of a call the path reading analyses
/// (module doc). A Deny is returned; an Ask recorded in `pending`.
pub(super) fn escaped_reference_verdict(
    ev: &Eval<'_>,
    depth: usize,
    ctx: &Ctx<'_>,
    pending: &mut Option<Decision>,
) -> Option<Decision> {
    let toks = ev.toks;
    for &(pos, end) in &ev.stmts {
        let st = &toks[pos..end];
        if matches!(name(st.first()), Some("import" | "from")) {
            continue;
        }
        // `x = os.remove` rewrites to `os.remove = os.remove` (an alias the
        // rewrite already followed into every use).
        if st.len() == 3 && st[1] == Tok::Punct('=') && name(st.first()) == name(st.get(2)) {
            continue;
        }
        for r in pos..end {
            let Tok::Name { text, .. } = &toks[r] else {
                continue;
            };
            if let Some(d) = lookup_verdict(ev, r, text, depth, ctx, pending) {
                return Some(d);
            }
            let attr = r > 0 && toks[r - 1] == Tok::Punct('.');
            let next = toks.get(r + 1);
            // A keyword argument name or an assignment target is a store, not
            // a reference.
            if next == Some(&Tok::Punct('=')) && toks.get(r + 2) != Some(&Tok::Punct('=')) {
                continue;
            }
            let Some((kind, i)) = find_ref(text, attr) else {
                continue;
            };
            let comps: Vec<&str> = text.split('.').collect();
            let after_is_call = comps[i + 1..].iter().all(|c| *c == "__call__");
            if after_is_call && next == Some(&Tok::Punct('(')) {
                continue; // a direct call: judged by `path_calls_verdict`
            }
            // `w = <reference>` whose every use is a direct call of `w`.
            if r + 1 == end && after_is_call {
                if let Some(w) = name(st.first()).filter(|_| st.get(1) == Some(&Tok::Punct('='))) {
                    let span_here = ev
                        .assigns
                        .get(w)
                        .is_some_and(|v| v.contains(&(pos + 2, end)));
                    if span_here && alias_fully_called(ev, w) {
                        continue;
                    }
                }
            }
            let shown = comps[..=i].join(".");
            match kind {
                Ref::Function => {
                    pending.get_or_insert(escaped(&shown));
                }
                Ref::Method => {
                    let m = comps[i];
                    let recv = if i == 0 {
                        // `<chain>.m`: a string-literal receiver is `str.m`.
                        if r >= 2 && matches!(toks[r - 2], Tok::Str { .. }) {
                            continue;
                        }
                        ev.receiver(r, m)
                    } else if attr {
                        None
                    } else {
                        let rn = comps[..i].join(".");
                        if PATH_CLASSES.contains(&rn.as_str()) {
                            pending.get_or_insert(escaped(&shown));
                            continue;
                        }
                        if string_type(ev, &rn) {
                            continue;
                        }
                        let t = top(&rn);
                        if ev.imported.contains(t) && !PATH_MODULES.contains(&t) {
                            continue;
                        }
                        ev.name_value_at(&rn, r)
                    };
                    if let Some(d) = judge_method_ref(&shown, m, recv, depth, ctx, pending) {
                        return Some(d);
                    }
                }
            }
        }
    }
    None
}

/// `getattr(recv, 'm')`, `operator.methodcaller('m', …)` and
/// `operator.attrgetter('a.m')` with a literal naming a write/move-capable
/// callable, at token `r`: judged as a reference to it.
fn lookup_verdict(
    ev: &Eval<'_>,
    r: usize,
    text: &str,
    depth: usize,
    ctx: &Ctx<'_>,
    pending: &mut Option<Decision>,
) -> Option<Decision> {
    let toks = ev.toks;
    if toks.get(r + 1) != Some(&Tok::Punct('(')) || (r > 0 && toks[r - 1] == Tok::Punct('.')) {
        return None;
    }
    let open = r + 1;
    let n = strip_call(text);
    let last = n.rsplit('.').next().unwrap_or(n);
    let lit = |idx: usize| ev.arg(open, idx, "").and_then(|(s, e)| ev.literal(s, e));
    match (n, last) {
        ("getattr" | "builtins.getattr", _) => {
            let m = lit(1)?;
            let (s, e) = ev.arg(open, 0, "")?;
            let recv_name = (e == s + 1).then(|| name(toks.get(s))).flatten();
            if let Some(rn) = recv_name {
                if PATH_FUNCTIONS.contains(&format!("{rn}.{m}").as_str())
                    || (PATH_CLASSES.contains(&rn) && PATH_METHODS.contains(&m.as_str()))
                {
                    pending.get_or_insert(escaped(&format!("getattr({rn}, '{m}')")));
                    return None;
                }
            }
            if !PATH_METHODS.contains(&m.as_str()) || ev.literal(s, e).is_some() {
                return None;
            }
            if let Some(rn) = recv_name {
                if string_type(ev, rn) {
                    return None;
                }
            }
            judge_method_ref(
                &format!("getattr(…, '{m}')"),
                &m,
                ev.expr(s, e),
                depth,
                ctx,
                pending,
            )
        }
        (_, "methodcaller") => {
            let m = lit(0)?;
            if PATH_METHODS.contains(&m.as_str()) {
                pending.get_or_insert(escaped(&format!("methodcaller('{m}')")));
            }
            None
        }
        (_, "attrgetter") => {
            let a = lit(0)?;
            if a.split('.').any(|c| PATH_METHODS.contains(&c)) {
                pending.get_or_insert(escaped(&format!("attrgetter('{a}')")));
            }
            None
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::{lex, Eval, Tok};
    use super::{alias_bindings, alias_fully_called, callees, find_ref, strip_call, Ref};

    /// The callees of the first call token whose text is `callee`.
    fn resolved(src: &str, callee: &str) -> Vec<(String, bool, bool)> {
        let toks = lex(src);
        let ev = Eval::new(&toks, src, None);
        let k = toks
            .iter()
            .position(|t| matches!(t, Tok::Name { text, .. } if text == callee))
            .unwrap();
        let attr = k > 0 && toks[k - 1] == Tok::Punct('.');
        callees(&ev, k, callee, attr)
            .into_iter()
            .map(|c| (c.n.to_string(), c.attr, c.unbound))
            .collect()
    }

    #[test]
    fn dunder_call_is_the_call_itself() {
        assert_eq!(strip_call("write_text.__call__.__call__"), "write_text");
        assert_eq!(strip_call("__call__"), "__call__");
        assert_eq!(
            resolved(
                "pathlib.Path('/x').write_text.__call__('y')",
                "write_text.__call__"
            ),
            vec![("write_text".to_string(), true, false)]
        );
    }

    #[test]
    fn unbound_pathlib_method_takes_its_receiver_from_the_arguments() {
        assert_eq!(
            resolved(
                "pathlib.Path.replace(pathlib.Path('a'), 'b')",
                "pathlib.Path.replace"
            ),
            vec![("pathlib.Path.replace".to_string(), false, true)]
        );
        // `str.replace` is not a pathlib class method.
        assert_eq!(
            resolved("str.replace('ab', 'a', 'c')", "str.replace"),
            vec![("str.replace".to_string(), false, false)]
        );
    }

    #[test]
    fn a_name_bound_only_to_references_is_read_as_the_referenced_call() {
        let src = "w = pathlib.Path('/x').write_text\nw('y')";
        assert_eq!(
            resolved(src, "w"),
            vec![("write_text".to_string(), true, false)]
        );
        let toks = lex(src);
        let ev = Eval::new(&toks, src, None);
        assert!(alias_fully_called(&ev, "w"));
    }

    #[test]
    fn a_name_with_any_other_binding_or_use_is_not_an_alias() {
        for src in [
            "w = print\nw = pathlib.Path('/x').write_text\nw('y')",
            "w = pathlib.Path('/x').write_text + 1\nw('y')",
            "for w in xs:\n  pass\nw = pathlib.Path('/x').write_text\nw('y')",
        ] {
            let toks = lex(src);
            let ev = Eval::new(&toks, src, None);
            assert!(alias_bindings(&ev, "w").is_none(), "{src}");
        }
        // Bound only to a reference, but also passed on: not fully called.
        let src = "w = pathlib.Path('/x').write_text\ng = w\ng('y')";
        let toks = lex(src);
        let ev = Eval::new(&toks, src, None);
        assert!(alias_bindings(&ev, "w").is_some());
        assert!(!alias_fully_called(&ev, "w"));
    }

    #[test]
    fn references_are_found_by_component() {
        assert!(matches!(
            find_ref("os.remove", false),
            Some((Ref::Function, 1))
        ));
        assert!(matches!(
            find_ref("os.replace", false),
            Some((Ref::Function, 1))
        ));
        assert!(matches!(find_ref("open", false), Some((Ref::Function, 0))));
        assert!(matches!(
            find_ref("p.write_text.__get__", false),
            Some((Ref::Method, 1))
        ));
        assert!(matches!(
            find_ref("write_text", true),
            Some((Ref::Method, 0))
        ));
        // A bare method name that is not an attribute is not a reference.
        assert!(find_ref("replace", false).is_none());
        assert!(find_ref("os.path.join", false).is_none());
    }
}
