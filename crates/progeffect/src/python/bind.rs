//! The binding pass: every name binding of the program, counted, so the
//! judging pass resolves a name only when the program binds it exactly one
//! way.

use std::collections::HashMap;

use rustpython_parser::ast::Expr;

use super::walk::{self, Bind, Visit};

/// Everything the program binds a name to.
#[derive(Default)]
pub(super) struct NameInfo {
    /// Qualified names of the imports binding it.
    pub imports: Vec<String>,
    /// Values of the module-level single-target assignments binding it.
    pub top: Vec<Expr>,
    /// Any other binding.
    pub other: usize,
}

/// What a name resolves to.
pub(super) enum Resolved<'a> {
    /// No binding in the program: a builtin, or a name the program never
    /// defines.
    Unbound,
    /// Bound only by imports, all of the same qualified name.
    Module(&'a str),
    /// Bound exactly once, by a module-level `name = value`.
    Value(&'a Expr),
    /// Bound some other way, or more than one way.
    Ambiguous { imported: bool },
}

#[derive(Default)]
pub(super) struct Bindings {
    names: HashMap<String, NameInfo>,
    pub failure: Option<String>,
}

impl Bindings {
    pub(super) fn collect(body: &[rustpython_parser::ast::Stmt]) -> Bindings {
        let mut b = Bindings::default();
        walk::module(&mut b, body);
        b
    }

    pub(super) fn resolve(&self, name: &str) -> Resolved<'_> {
        let Some(info) = self.names.get(name) else {
            return Resolved::Unbound;
        };
        if let Some(first) = info.imports.first() {
            if info.other == 0 && info.top.is_empty() && info.imports.iter().all(|q| q == first) {
                return Resolved::Module(first);
            }
            return Resolved::Ambiguous { imported: true };
        }
        match (info.top.as_slice(), info.other) {
            ([v], 0) => Resolved::Value(v),
            _ => Resolved::Ambiguous { imported: false },
        }
    }
}

impl Visit for Bindings {
    fn bind(&mut self, name: &str, how: Bind<'_>) {
        let info = self.names.entry(name.to_string()).or_default();
        match how {
            Bind::Import(q) => info.imports.push(q),
            Bind::TopAssign(v) => info.top.push(v.clone()),
            Bind::Other => info.other += 1,
        }
    }

    fn fail(&mut self, why: String) {
        self.failure.get_or_insert(why);
    }
}
