//! One exhaustive traversal of a Python AST, shared by the binding pass
//! ([`super::bind`]) and the judging pass ([`super::judge`]).
//!
//! Every `match` here names every variant (no `_` arm), so a node kind added
//! to the parser is a compile error, not a silently skipped subtree. The
//! generated `rustpython_ast::Visitor` is NOT used on purpose: its default
//! `generic_visit_comprehension`, `_keyword`, `_withitem`, `_arguments` and
//! `_match_case` are empty, so `[os.system(c) for c in x]`,
//! `f(k=os.system(c))` and `with open(p, 'w'):` would never be seen.

use rustpython_parser::ast::{
    self, Arguments, Comprehension, ExceptHandler, Expr, ExprAttribute, ExprCall, ExprContext,
    ExprName, ExprSubscript, Keyword, MatchCase, Pattern, Stmt, TypeParam, WithItem,
};

/// Nesting depth (statements + expressions) past which the program is
/// Undetermined rather than walked further (bounded recursion).
pub(super) const MAX_DEPTH: usize = 200;

/// How a name is bound.
pub(super) enum Bind<'a> {
    /// `import a.b` binds `a` to `a`; `import a.b as x` binds `x` to `a.b`;
    /// `from a import b [as x]` binds `b` / `x` to `a.b`.
    Import(String),
    /// `name = value` as a statement directly in the module body, with that
    /// single target.
    TopAssign(&'a Expr),
    /// Any other binding (assignment elsewhere, loop / with / except /
    /// comprehension / walrus target, parameter, def / class name, `del`,
    /// `global` / `nonlocal`, a match capture, a type parameter).
    Other,
}

/// The hooks a pass implements. Every hook defaults to doing nothing.
pub(super) trait Visit {
    /// A name is bound.
    fn bind(&mut self, _name: &str, _how: Bind<'_>) {}
    /// The traversal cannot continue (too deep, an import form not modelled,
    /// a non-ASCII identifier). The pass must end Undetermined.
    fn fail(&mut self, why: String);
    /// A call (its callee and arguments are walked separately, after this
    /// hook).
    fn call(&mut self, _c: &ExprCall) {}
    /// The value of an expression statement, whose result is discarded (the
    /// value itself is walked separately, after this hook).
    fn discarded(&mut self, _e: &Expr) {}
    /// A name read as a value (not as a callee and not as the base of an
    /// attribute access).
    fn name_value(&mut self, _n: &ExprName) {}
    /// A name used as a callee or as the base of an attribute access.
    fn name_head(&mut self, _n: &ExprName) {}
    /// An attribute read as a value, or stored / deleted (`ctx` says which).
    fn attr_value(&mut self, _a: &ExprAttribute) {}
    /// An attribute used as a callee or as the base of another attribute.
    fn attr_head(&mut self, _a: &ExprAttribute) {}
    /// A subscript stored into or deleted (`x[k] = v`, `del x[k]`).
    fn subscript_store(&mut self, _s: &ExprSubscript) {}
}

/// Walk a module body.
pub(super) fn module<V: Visit>(v: &mut V, body: &[Stmt]) {
    let mut w = Walker { v, depth: 0 };
    for s in body {
        w.stmt(s, true);
    }
}

struct Walker<'v, V: Visit> {
    v: &'v mut V,
    depth: usize,
}

fn ascii_ident(v: &mut impl Visit, id: &str) {
    // CPython NFKC-normalises identifiers (`ｓｙｓｔｅｍ` is `system`); this
    // reading compares names byte for byte, so a non-ASCII identifier is not
    // read at all.
    if !id.is_ascii() {
        v.fail(format!(
            "the identifier `{id}` is not ASCII (Python normalises it with NFKC)"
        ));
    }
}

impl<V: Visit> Walker<'_, V> {
    fn enter(&mut self) -> bool {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            self.v
                .fail(format!("the program nests deeper than {MAX_DEPTH} levels"));
            return false;
        }
        true
    }

    fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    fn bind(&mut self, id: &str, how: Bind<'_>) {
        ascii_ident(self.v, id);
        self.v.bind(id, how);
    }

    fn stmts(&mut self, body: &[Stmt]) {
        for s in body {
            self.stmt(s, false);
        }
    }

    fn stmt(&mut self, s: &Stmt, top: bool) {
        if !self.enter() {
            return;
        }
        match s {
            Stmt::FunctionDef(d) => {
                self.exprs(&d.decorator_list);
                self.arguments(&d.args);
                self.opt(d.returns.as_deref());
                self.type_params(&d.type_params);
                self.bind(d.name.as_str(), Bind::Other);
                self.stmts(&d.body);
            }
            Stmt::AsyncFunctionDef(d) => {
                self.exprs(&d.decorator_list);
                self.arguments(&d.args);
                self.opt(d.returns.as_deref());
                self.type_params(&d.type_params);
                self.bind(d.name.as_str(), Bind::Other);
                self.stmts(&d.body);
            }
            Stmt::ClassDef(d) => {
                self.exprs(&d.decorator_list);
                self.exprs(&d.bases);
                self.keywords(&d.keywords);
                self.type_params(&d.type_params);
                self.bind(d.name.as_str(), Bind::Other);
                self.stmts(&d.body);
            }
            Stmt::Return(r) => self.opt(r.value.as_deref()),
            Stmt::Delete(d) => {
                for t in &d.targets {
                    self.target(t);
                }
            }
            Stmt::Assign(a) => {
                self.expr(&a.value);
                match (top, a.targets.as_slice()) {
                    (true, [Expr::Name(n)]) => self.bind(n.id.as_str(), Bind::TopAssign(&a.value)),
                    _ => {
                        for t in &a.targets {
                            self.target(t);
                        }
                    }
                }
            }
            Stmt::TypeAlias(t) => {
                self.type_params(&t.type_params);
                self.expr(&t.value);
                self.target(&t.name);
            }
            Stmt::AugAssign(a) => {
                self.expr(&a.value);
                // `x += v` reads x too, then binds it.
                self.expr_load_of_target(&a.target);
                self.target(&a.target);
            }
            Stmt::AnnAssign(a) => {
                self.expr(&a.annotation);
                self.opt(a.value.as_deref());
                self.target(&a.target);
            }
            Stmt::For(f) => {
                self.expr(&f.iter);
                self.target(&f.target);
                self.stmts(&f.body);
                self.stmts(&f.orelse);
            }
            Stmt::AsyncFor(f) => {
                self.expr(&f.iter);
                self.target(&f.target);
                self.stmts(&f.body);
                self.stmts(&f.orelse);
            }
            Stmt::While(w) => {
                self.expr(&w.test);
                self.stmts(&w.body);
                self.stmts(&w.orelse);
            }
            Stmt::If(i) => {
                self.expr(&i.test);
                self.stmts(&i.body);
                self.stmts(&i.orelse);
            }
            Stmt::With(w) => {
                self.with_items(&w.items);
                self.stmts(&w.body);
            }
            Stmt::AsyncWith(w) => {
                self.with_items(&w.items);
                self.stmts(&w.body);
            }
            Stmt::Match(m) => {
                self.expr(&m.subject);
                for c in &m.cases {
                    self.match_case(c);
                }
            }
            Stmt::Raise(r) => {
                self.opt(r.exc.as_deref());
                self.opt(r.cause.as_deref());
            }
            Stmt::Try(t) => {
                self.stmts(&t.body);
                self.handlers(&t.handlers);
                self.stmts(&t.orelse);
                self.stmts(&t.finalbody);
            }
            Stmt::TryStar(t) => {
                self.stmts(&t.body);
                self.handlers(&t.handlers);
                self.stmts(&t.orelse);
                self.stmts(&t.finalbody);
            }
            Stmt::Assert(a) => {
                self.expr(&a.test);
                self.opt(a.msg.as_deref());
            }
            Stmt::Import(i) => {
                for alias in &i.names {
                    let name = alias.name.as_str();
                    ascii_ident(self.v, name);
                    match &alias.asname {
                        Some(as_) => self.bind(as_.as_str(), Bind::Import(name.to_string())),
                        None => {
                            let head = name.split('.').next().unwrap_or(name);
                            self.bind(head, Bind::Import(head.to_string()));
                        }
                    }
                }
            }
            Stmt::ImportFrom(i) => {
                let level = i.level.as_ref().map_or(0, ast::Int::to_u32);
                let Some(module) = i.module.as_ref().filter(|_| level == 0) else {
                    self.v
                        .fail("a relative import (`from . import …`) loads a file of the working directory".into());
                    self.leave();
                    return;
                };
                ascii_ident(self.v, module.as_str());
                for alias in &i.names {
                    let name = alias.name.as_str();
                    if name == "*" {
                        self.v.fail(format!(
                            "`from {module} import *` binds names this reading cannot list"
                        ));
                        continue;
                    }
                    let bound = alias.asname.as_ref().map_or(name, |a| a.as_str());
                    self.bind(bound, Bind::Import(format!("{module}.{name}")));
                }
            }
            Stmt::Global(g) => {
                for n in &g.names {
                    self.bind(n.as_str(), Bind::Other);
                }
            }
            Stmt::Nonlocal(g) => {
                for n in &g.names {
                    self.bind(n.as_str(), Bind::Other);
                }
            }
            Stmt::Expr(e) => {
                self.v.discarded(&e.value);
                self.expr(&e.value);
            }
            Stmt::Pass(_) | Stmt::Break(_) | Stmt::Continue(_) => {}
        }
        self.leave();
    }

    fn handlers(&mut self, hs: &[ExceptHandler]) {
        for h in hs {
            match h {
                ExceptHandler::ExceptHandler(h) => {
                    self.opt(h.type_.as_deref());
                    if let Some(n) = &h.name {
                        self.bind(n.as_str(), Bind::Other);
                    }
                    self.stmts(&h.body);
                }
            }
        }
    }

    fn with_items(&mut self, items: &[WithItem]) {
        for i in items {
            self.expr(&i.context_expr);
            if let Some(t) = &i.optional_vars {
                self.target(t);
            }
        }
    }

    fn match_case(&mut self, c: &MatchCase) {
        self.pattern(&c.pattern);
        self.opt(c.guard.as_deref());
        self.stmts(&c.body);
    }

    fn pattern(&mut self, p: &Pattern) {
        if !self.enter() {
            return;
        }
        match p {
            Pattern::MatchValue(v) => self.expr(&v.value),
            Pattern::MatchSingleton(_) => {}
            Pattern::MatchSequence(s) => {
                for p in &s.patterns {
                    self.pattern(p);
                }
            }
            Pattern::MatchMapping(m) => {
                self.exprs(&m.keys);
                for p in &m.patterns {
                    self.pattern(p);
                }
                if let Some(r) = &m.rest {
                    self.bind(r.as_str(), Bind::Other);
                }
            }
            Pattern::MatchClass(c) => {
                self.expr(&c.cls);
                for p in &c.patterns {
                    self.pattern(p);
                }
                for a in &c.kwd_attrs {
                    ascii_ident(self.v, a.as_str());
                }
                for p in &c.kwd_patterns {
                    self.pattern(p);
                }
            }
            Pattern::MatchStar(s) => {
                if let Some(n) = &s.name {
                    self.bind(n.as_str(), Bind::Other);
                }
            }
            Pattern::MatchAs(a) => {
                if let Some(p) = &a.pattern {
                    self.pattern(p);
                }
                if let Some(n) = &a.name {
                    self.bind(n.as_str(), Bind::Other);
                }
            }
            Pattern::MatchOr(o) => {
                for p in &o.patterns {
                    self.pattern(p);
                }
            }
        }
        self.leave();
    }

    fn type_params(&mut self, ps: &[TypeParam]) {
        for p in ps {
            match p {
                TypeParam::TypeVar(t) => {
                    self.opt(t.bound.as_deref());
                    self.bind(t.name.as_str(), Bind::Other);
                }
                TypeParam::ParamSpec(t) => self.bind(t.name.as_str(), Bind::Other),
                TypeParam::TypeVarTuple(t) => self.bind(t.name.as_str(), Bind::Other),
            }
        }
    }

    fn arguments(&mut self, a: &Arguments) {
        for arg in a.posonlyargs.iter().chain(&a.args).chain(&a.kwonlyargs) {
            self.opt(arg.default.as_deref());
            self.opt(arg.def.annotation.as_deref());
            self.bind(arg.def.arg.as_str(), Bind::Other);
        }
        for arg in a.vararg.iter().chain(a.kwarg.iter()) {
            self.opt(arg.annotation.as_deref());
            self.bind(arg.arg.as_str(), Bind::Other);
        }
    }

    fn keywords(&mut self, ks: &[Keyword]) {
        for k in ks {
            if let Some(a) = &k.arg {
                ascii_ident(self.v, a.as_str());
            }
            self.expr(&k.value);
        }
    }

    fn comprehensions(&mut self, gs: &[Comprehension]) {
        for g in gs {
            self.expr(&g.iter);
            self.target(&g.target);
            self.exprs(&g.ifs);
        }
    }

    fn opt(&mut self, e: Option<&Expr>) {
        if let Some(e) = e {
            self.expr(e);
        }
    }

    fn exprs(&mut self, es: &[Expr]) {
        for e in es {
            self.expr(e);
        }
    }

    /// An assignment / loop / `del` target: names are bound, attribute and
    /// subscript targets are stores into an object (their bases are read).
    fn target(&mut self, t: &Expr) {
        if !self.enter() {
            return;
        }
        match t {
            Expr::Name(n) => self.bind(n.id.as_str(), Bind::Other),
            Expr::Tuple(x) => {
                for e in &x.elts {
                    self.target(e);
                }
            }
            Expr::List(x) => {
                for e in &x.elts {
                    self.target(e);
                }
            }
            Expr::Starred(s) => self.target(&s.value),
            // Any other target (attribute, subscript) is an expression whose
            // context is Store / Del: walked as one.
            other => self.expr(other),
        }
        self.leave();
    }

    /// The read half of an augmented assignment target.
    fn expr_load_of_target(&mut self, t: &Expr) {
        // Attribute / subscript targets are walked by `target` as stores;
        // their bases are read there.
        if let Expr::Name(n) = t {
            ascii_ident(self.v, n.id.as_str());
            self.v.name_value(n);
        }
    }

    fn expr(&mut self, e: &Expr) {
        if !self.enter() {
            return;
        }
        match e {
            Expr::BoolOp(b) => self.exprs(&b.values),
            Expr::NamedExpr(n) => {
                self.expr(&n.value);
                self.target(&n.target);
            }
            Expr::BinOp(b) => {
                self.expr(&b.left);
                self.expr(&b.right);
            }
            Expr::UnaryOp(u) => self.expr(&u.operand),
            Expr::Lambda(l) => {
                self.arguments(&l.args);
                self.expr(&l.body);
            }
            Expr::IfExp(i) => {
                self.expr(&i.test);
                self.expr(&i.body);
                self.expr(&i.orelse);
            }
            Expr::Dict(d) => {
                for k in d.keys.iter().flatten() {
                    self.expr(k);
                }
                self.exprs(&d.values);
            }
            Expr::Set(s) => self.exprs(&s.elts),
            Expr::ListComp(c) => {
                self.comprehensions(&c.generators);
                self.expr(&c.elt);
            }
            Expr::SetComp(c) => {
                self.comprehensions(&c.generators);
                self.expr(&c.elt);
            }
            Expr::DictComp(c) => {
                self.comprehensions(&c.generators);
                self.expr(&c.key);
                self.expr(&c.value);
            }
            Expr::GeneratorExp(c) => {
                self.comprehensions(&c.generators);
                self.expr(&c.elt);
            }
            Expr::Await(a) => self.expr(&a.value),
            Expr::Yield(y) => self.opt(y.value.as_deref()),
            Expr::YieldFrom(y) => self.expr(&y.value),
            Expr::Compare(c) => {
                self.expr(&c.left);
                self.exprs(&c.comparators);
            }
            Expr::Call(c) => {
                self.v.call(c);
                self.head(&c.func);
                self.exprs(&c.args);
                self.keywords(&c.keywords);
            }
            Expr::FormattedValue(f) => {
                self.expr(&f.value);
                self.opt(f.format_spec.as_deref());
            }
            Expr::JoinedStr(j) => self.exprs(&j.values),
            Expr::Constant(_) => {}
            Expr::Attribute(a) => {
                ascii_ident(self.v, a.attr.as_str());
                self.v.attr_value(a);
                self.head(&a.value);
            }
            Expr::Subscript(s) => {
                if !matches!(s.ctx, ExprContext::Load) {
                    self.v.subscript_store(s);
                }
                self.expr(&s.value);
                self.expr(&s.slice);
            }
            Expr::Starred(s) => self.expr(&s.value),
            Expr::Name(n) => {
                ascii_ident(self.v, n.id.as_str());
                match n.ctx {
                    ExprContext::Load => self.v.name_value(n),
                    ExprContext::Store | ExprContext::Del => self.bind(n.id.as_str(), Bind::Other),
                }
            }
            Expr::List(l) => self.exprs(&l.elts),
            Expr::Tuple(t) => self.exprs(&t.elts),
            Expr::Slice(s) => {
                self.opt(s.lower.as_deref());
                self.opt(s.upper.as_deref());
                self.opt(s.step.as_deref());
            }
        }
        self.leave();
    }

    /// A callee, or the base of an attribute access.
    fn head(&mut self, e: &Expr) {
        if !self.enter() {
            return;
        }
        match e {
            Expr::Name(n) => {
                ascii_ident(self.v, n.id.as_str());
                self.v.name_head(n);
            }
            Expr::Attribute(a) => {
                ascii_ident(self.v, a.attr.as_str());
                self.v.attr_head(a);
                self.head(&a.value);
            }
            other => self.expr(other),
        }
        self.leave();
    }
}
