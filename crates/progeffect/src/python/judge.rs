//! The judging pass: every call of the program classified into [`Effect`]s,
//! and every construct that defeats the reading turned into a failure
//! (Undetermined).

use rustpython_parser::ast::{Expr, ExprAttribute, ExprCall, ExprContext, ExprName, ExprSubscript};
use rustpython_parser::text_size::TextRange;

use super::bind::{Bindings, Resolved};
use super::tables::{
    DANGEROUS_BUILTINS, EXEC_NAMES, FILE_WRITER_NAMES, FS_METHOD_NAMES, HARMLESS_DUNDERS,
    NAMESPACE_BUILTINS, READ_ONLY, SAFE_MODULES, STDLIB, SUBPROCESS_PLAIN_KEYWORDS,
    TEMPFILE_CREATORS,
};
use super::value::{bool_lit, int, Cx};
use super::walk::{Bind, Visit};
use crate::{Effect, FsOp};

/// Effects one program may report before it is Undetermined (bounds the
/// host's work: each effect is judged on its own).
pub(super) const MAX_EFFECTS: usize = 64;

pub(super) struct Judge<'a> {
    pub cx: Cx<'a>,
    pub effects: Vec<Effect>,
    pub failure: Option<String>,
    /// Ranges of the `globals` / `locals` / `vars` / `compile` callee names
    /// whose call result is only displayed or discarded (see
    /// [`Judge::inert_result`]), and of a bare `globals` / `locals` / `vars`
    /// that is a whole positional `print` argument ([`Judge::display_args`]);
    /// every other reference to one of them stays Undetermined.
    inert_ns: Vec<TextRange>,
}

impl<'a> Judge<'a> {
    pub(super) fn new(b: &'a Bindings, argv: &'a [Option<String>], src: &'a str) -> Judge<'a> {
        Judge {
            cx: Cx { b, argv, src },
            effects: Vec::new(),
            failure: None,
            inert_ns: Vec::new(),
        }
    }

    /// User rulings 2026-11-19: a namespace builtin (`globals()`, `locals()`,
    /// `vars(x)`) whose result is only displayed and never called is not
    /// dynamic dispatch, and a `compile(...)` whose result is never passed to
    /// `exec` / `eval`, never bound and never called runs nothing (whatever
    /// source it compiles).
    ///
    /// `e` is an expression in an inert position: a positional argument of
    /// the unbound builtin `print` ([`Judge::display_args`]) or the value of
    /// an expression statement (its result is discarded). When `e` is a call
    /// of an unbound [`NAMESPACE_BUILTINS`] name (no keywords, no `*`, no
    /// argument for `globals` / `locals`, at most one for `vars`) or of an
    /// unbound `compile` (any arguments), optionally subscripted any number
    /// of times (`print(globals()["x"])`), its callee name is recorded as
    /// inert: nothing in that shape calls a value fetched from the namespace
    /// or runs the compiled code. Anything else — a call on a fetched value
    /// (`globals()["x"]()`, `compile(...)()`), a binding (`g = globals()`,
    /// `c = compile(...)`), any other enclosing call (`exec(compile(...))`),
    /// `.get` / `.values()` — is not matched and stays Undetermined in
    /// `name_head`. The subscript indices and the call's own arguments are
    /// walked and judged as usual.
    fn inert_result(&mut self, e: &Expr) {
        let mut e = e;
        while let Expr::Subscript(s) = e {
            if !matches!(s.ctx, ExprContext::Load) {
                return;
            }
            e = &s.value;
        }
        let Expr::Call(inner) = e else {
            return;
        };
        let Expr::Name(n) = inner.func.as_ref() else {
            return;
        };
        let id = n.id.as_str();
        if !matches!(self.cx.b.resolve(id), Resolved::Unbound) {
            return;
        }
        let max_args = usize::from(id == "vars");
        let namespace = NAMESPACE_BUILTINS.contains(&id)
            && inner.keywords.is_empty()
            && inner.args.len() <= max_args
            && !inner.args.iter().any(|x| matches!(x, Expr::Starred(_)));
        if namespace || id == "compile" {
            self.inert_ns.push(n.range);
        }
    }

    /// The positional arguments of a call of the unbound builtin `print`
    /// are inert positions (they are only displayed).
    ///
    /// Besides the call shapes of [`Judge::inert_result`], an argument that is
    /// the bare unbound [`NAMESPACE_BUILTINS`] name itself (`print(vars)`,
    /// backlog 20261115) is inert: printing the builtin function object calls
    /// nothing and fetches nothing. Only a whole positional argument matches —
    /// a starred (`print(*vars)`), keyword (`print(sep=vars)`), nested
    /// (`print((vars, 1))`), attribute (`print(vars.x)`) or bound use stays
    /// Undetermined in `name_head`.
    fn display_args(&mut self, c: &ExprCall) {
        let Expr::Name(f) = c.func.as_ref() else {
            return;
        };
        if f.id.as_str() != "print" || !matches!(self.cx.b.resolve("print"), Resolved::Unbound) {
            return;
        }
        for a in &c.args {
            if let Expr::Name(n) = a {
                let id = n.id.as_str();
                if NAMESPACE_BUILTINS.contains(&id)
                    && matches!(n.ctx, ExprContext::Load)
                    && matches!(self.cx.b.resolve(id), Resolved::Unbound)
                {
                    self.inert_ns.push(n.range);
                }
                continue;
            }
            self.inert_result(a);
        }
    }

    fn undetermined(&mut self, why: impl Into<String>) {
        self.failure.get_or_insert_with(|| why.into());
    }

    fn push(&mut self, call: &str, op: FsOp) {
        self.effect(Effect::Fs {
            call: call.to_string(),
            op,
        });
    }

    fn effect(&mut self, e: Effect) {
        if self.effects.len() >= MAX_EFFECTS {
            self.undetermined(format!("the program has more than {MAX_EFFECTS} effects"));
            return;
        }
        self.effects.push(e);
    }
}

fn is_dunder(s: &str) -> bool {
    s.len() > 4 && s.starts_with("__") && s.ends_with("__")
}

fn is_constant_name(s: &str) -> bool {
    s.chars().any(|c| c.is_ascii_uppercase())
        && s.chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

fn matches_table(q: &str, table: &[&str]) -> bool {
    table.iter().any(|t| match t.strip_suffix('.') {
        Some(prefix) => q == prefix || q.starts_with(t),
        None => q == *t,
    })
}

fn last(q: &str) -> &str {
    q.rsplit('.').next().unwrap_or(q)
}

fn top(q: &str) -> &str {
    q.split('.').next().unwrap_or(q)
}

/// Attribute values of effect-capable modules that are plain data.
const VALUE_ATTRS: &[&str] = &[
    "sys.argv",
    "sys.version",
    "sys.version_info",
    "sys.platform",
    "sys.executable",
    "sys.maxsize",
    "sys.byteorder",
    "sys.stdout",
    "sys.stderr",
    "sys.stdin",
    "os.environ",
    "os.name",
    "os.pathsep",
    "os.curdir",
    "os.pardir",
    "os.extsep",
    "os.altsep",
];

/// A module / attribute referenced as a VALUE (not called) has no effect
/// when it is data or a read-only function; anything else — `os`, an
/// effect-capable function such as `os.system` — escapes the reading (it may
/// be called later under another name).
fn inert_value(q: &str) -> bool {
    let name = last(q);
    if EXEC_NAMES.contains(&name) || FS_METHOD_NAMES.contains(&name) {
        return false;
    }
    SAFE_MODULES.contains(&top(q))
        || matches_table(q, READ_ONLY)
        || VALUE_ATTRS.contains(&q)
        || is_constant_name(name)
}

/// Positional argument `i`, or the keyword named one of `names`.
fn arg<'e>(c: &'e ExprCall, i: usize, names: &[&str]) -> Option<&'e Expr> {
    c.args.get(i).or_else(|| {
        c.keywords
            .iter()
            .find(|k| k.arg.as_ref().is_some_and(|a| names.contains(&a.as_str())))
            .map(|k| &k.value)
    })
}

/// `*args` / `**kwargs` in a call: its argument list is not determined.
fn splatted(c: &ExprCall) -> bool {
    c.args.iter().any(|a| matches!(a, Expr::Starred(_)))
        || c.keywords.iter().any(|k| k.arg.is_none())
}

fn has_keyword(c: &ExprCall, name: &str) -> bool {
    c.keywords
        .iter()
        .any(|k| k.arg.as_ref().is_some_and(|a| a.as_str() == name))
}

/// The effect a literal `open` mode has: `None` for a read.
fn mode_effect(mode: &str, path: String) -> Option<FsOp> {
    if mode.contains('a') {
        Some(FsOp::Append { path })
    } else if mode.contains(['w', 'x', '+']) {
        Some(FsOp::Truncate { path })
    } else {
        None
    }
}

impl Judge<'_> {
    fn is_path_obj(&self, e: &Expr, depth: usize) -> bool {
        if depth > 16 {
            return false;
        }
        match e {
            Expr::Call(c) => self.cx.qual(&c.func).is_some_and(|q| {
                matches!(
                    q.as_str(),
                    "pathlib.Path"
                        | "pathlib.PurePath"
                        | "pathlib.PosixPath"
                        | "pathlib.PurePosixPath"
                )
            }),
            Expr::BinOp(b) => self.is_path_obj(&b.left, depth + 1),
            Expr::Name(n) => match self.cx.b.resolve(n.id.as_str()) {
                Resolved::Value(v) => self.is_path_obj(v, depth + 1),
                _ => false,
            },
            _ => false,
        }
    }

    /// A path operand: determined, else the program is Undetermined.
    fn path_operand(&mut self, call: &str, e: Option<&Expr>) -> Option<String> {
        let p = e.and_then(|e| self.cx.path(e));
        let Some(p) = p else {
            self.undetermined(format!(
                "`{call}` is called on a path the program text does not determine"
            ));
            return None;
        };
        // A non-ASCII DIRECTORY component in a program that uses a `\N{…}`
        // escape: the pre-S5 reading (backlog 47b20647) pinned
        // `open('/\N{SNOWMAN}tc/hosts','w')` as undetermined (Ask / Deny), and
        // this reading does not lower a pinned verdict on its own authority
        // (the decoded path would be judged Allow). Restrictive only; a
        // non-ASCII FILE name (`/tmp/\N{SNOWMAN}`) is read as decoded.
        let dir_non_ascii = p.rsplit_once('/').is_some_and(|(dir, _)| !dir.is_ascii());
        if dir_non_ascii && self.cx.has_named_escape() {
            self.undetermined(format!(
                "`{call}` is called on a path whose directory is spelt with a `\\N{{…}}` escape \
                 to a non-ASCII character"
            ));
            return None;
        }
        Some(p)
    }

    /// `open`-like call: `path` / `mode` expressions as given.
    fn open_like(&mut self, call: &str, c: &ExprCall, path: Option<&Expr>, mode: Option<&Expr>) {
        if has_keyword(c, "opener") {
            self.undetermined(format!("`{call}` is given an `opener`"));
            return;
        }
        let mode = match mode {
            None => return,
            Some(m) => match self.cx.string(m) {
                Some(m) => m,
                None => {
                    self.undetermined(format!(
                        "`{call}` is called with a mode the program text does not determine"
                    ));
                    return;
                }
            },
        };
        if mode_effect(&mode, String::new()).is_none() {
            return;
        }
        // A file descriptor (`open(1, 'w')`) was opened elsewhere, where it
        // is judged; it names no path.
        if path.is_some_and(|p| int(p).is_some()) {
            return;
        }
        if let Some(p) = self.path_operand(call, path) {
            if let Some(op) = mode_effect(&mode, p) {
                self.push(call, op);
            }
        }
    }

    fn shell_string(&mut self, call: &str, e: Option<&Expr>) {
        match e.and_then(|e| self.cx.string(e)) {
            Some(command) => self.effect(Effect::Shell {
                call: call.to_string(),
                command,
            }),
            None => self.undetermined(format!(
                "`{call}` runs a shell command the program text does not determine"
            )),
        }
    }

    fn subprocess(&mut self, call: &str, c: &ExprCall) {
        for k in &c.keywords {
            let Some(name) = k.arg.as_ref().map(|a| a.as_str()) else {
                continue;
            };
            if name != "args" && !SUBPROCESS_PLAIN_KEYWORDS.contains(&name) {
                self.undetermined(format!(
                    "`{call}` is given `{name}=`, which changes what or how it runs"
                ));
                return;
            }
        }
        if c.args.len() > 1 {
            self.undetermined(format!(
                "`{call}` is given positional arguments after the command"
            ));
            return;
        }
        let shell = match c
            .keywords
            .iter()
            .find(|k| k.arg.as_ref().is_some_and(|a| a.as_str() == "shell"))
        {
            None => false,
            Some(k) => match bool_lit(&k.value) {
                Some(b) => b,
                None => {
                    self.undetermined(format!("`{call}`'s `shell=` is not a literal True / False"));
                    return;
                }
            },
        };
        let Some(args) = arg(c, 0, &["args"]) else {
            self.undetermined(format!("`{call}` is called without a command"));
            return;
        };
        let list = self.cx.strings(args);
        let argv = match (shell, list) {
            (true, Some(list)) => {
                let mut argv = vec!["sh".to_string(), "-c".to_string()];
                argv.extend(list);
                argv
            }
            (true, None) => return self.shell_string(call, Some(args)),
            (false, Some(list)) => list,
            (false, None) => match self.cx.string(args) {
                Some(s) => vec![s],
                None => Vec::new(),
            },
        };
        if argv.len() < if shell { 3 } else { 1 } {
            self.undetermined(format!(
                "`{call}` runs a command the program text does not determine"
            ));
            return;
        }
        self.effect(Effect::Exec {
            call: call.to_string(),
            argv,
        });
    }

    fn two_paths(&mut self, call: &str, c: &ExprCall, f: impl FnOnce(String, String) -> FsOp) {
        let a = self.path_operand(call, arg(c, 0, &["src", "source", "path"]));
        let b = self.path_operand(call, arg(c, 1, &["dst", "destination", "target"]));
        if let (Some(a), Some(b)) = (a, b) {
            self.push(call, f(a, b));
        }
    }

    fn one_path(&mut self, call: &str, c: &ExprCall, f: impl FnOnce(String) -> FsOp) {
        if let Some(p) = self.path_operand(call, arg(c, 0, &["path", "name", "file"])) {
            self.push(call, f(p));
        }
    }

    /// A call whose callee resolves through imports to the qualified name `q`.
    fn module_call(&mut self, q: &str, c: &ExprCall) {
        if splatted(c) && !SAFE_MODULES.contains(&top(q)) && !matches_table(q, READ_ONLY) {
            self.undetermined(format!("`{q}` is called with `*args` / `**kwargs`"));
            return;
        }
        if c.keywords.iter().any(|k| {
            k.arg
                .as_ref()
                .is_some_and(|a| a.as_str().ends_with("dir_fd"))
        }) {
            self.undetermined(format!("`{q}` is given a `dir_fd`"));
            return;
        }
        match q {
            "os.system" | "posix.system" => self.shell_string(q, arg(c, 0, &["command"])),
            "subprocess.getoutput" | "subprocess.getstatusoutput" => {
                self.shell_string(q, arg(c, 0, &["cmd"]));
            }
            "os.popen" => {
                let mode = arg(c, 1, &["mode"]).map(|m| self.cx.string(m));
                match mode {
                    None => {}
                    Some(Some(m)) if !m.contains(['w', 'a', '+']) => {}
                    Some(_) => {
                        self.undetermined("`os.popen` writes to the command's stdin");
                        return;
                    }
                }
                self.shell_string(q, arg(c, 0, &["cmd"]));
            }
            "subprocess.run"
            | "subprocess.call"
            | "subprocess.check_call"
            | "subprocess.check_output"
            | "subprocess.Popen" => self.subprocess(q, c),
            "builtins.open" | "io.open" | "io.FileIO" | "codecs.open" | "gzip.open"
            | "bz2.open" | "lzma.open" => {
                self.open_like(q, c, arg(c, 0, &["file", "filename"]), arg(c, 1, &["mode"]))
            }
            "os.open" => {
                let read_only = arg(c, 1, &["flags"]).is_some_and(|f| {
                    self.cx.qual(f).as_deref() == Some("os.O_RDONLY") || int(f) == Some(0)
                });
                if !read_only {
                    self.one_path(q, c, |path| FsOp::Truncate { path });
                }
            }
            "os.fdopen" => {}
            "os.remove" | "os.unlink" | "posix.remove" | "posix.unlink" => {
                self.one_path(q, c, |path| FsOp::Delete {
                    path,
                    recursive: false,
                });
            }
            "os.rmdir" => self.one_path(q, c, |path| FsOp::RemoveDir { path }),
            "shutil.rmtree" => self.one_path(q, c, |path| FsOp::Delete {
                path,
                recursive: true,
            }),
            "os.rename" | "os.replace" | "os.renames" | "shutil.move" => {
                self.two_paths(q, c, |from, to| FsOp::Move { from, to });
            }
            "shutil.copy" | "shutil.copy2" | "shutil.copyfile" => {
                self.two_paths(q, c, |from, to| FsOp::Copy {
                    from,
                    to,
                    recursive: false,
                });
            }
            "shutil.copytree" => self.two_paths(q, c, |from, to| FsOp::Copy {
                from,
                to,
                recursive: true,
            }),
            "os.symlink" => self.two_paths(q, c, |target, link| FsOp::Link {
                target,
                link,
                symbolic: true,
            }),
            "os.link" => self.two_paths(q, c, |target, link| FsOp::Link {
                target,
                link,
                symbolic: false,
            }),
            "os.mkdir" | "os.makedirs" => self.one_path(q, c, |path| FsOp::MakeDir { path }),
            "os.utime" => self.one_path(q, c, |path| FsOp::Touch { path }),
            "os.chmod" | "os.lchmod" => {
                let mode = arg(c, 1, &["mode"])
                    .and_then(int)
                    .and_then(|m| u32::try_from(m).ok());
                match mode {
                    Some(mode) => self.one_path(q, c, |path| FsOp::Chmod { path, mode }),
                    None => self.undetermined(format!("`{q}`'s mode is not an integer literal")),
                }
            }
            "os.truncate" => {
                let size = arg(c, 1, &["length"])
                    .and_then(int)
                    .and_then(|m| u64::try_from(m).ok());
                match size {
                    Some(size) => self.one_path(q, c, |path| FsOp::Resize { path, size }),
                    None => self.undetermined(format!("`{q}`'s length is not an integer literal")),
                }
            }
            "os.chown" | "os.lchown" | "shutil.chown" => self.chown(q, c),
            _ => self.other_module_call(q, c),
        }
    }

    fn chown(&mut self, q: &str, c: &ExprCall) {
        let part = |e: Option<&Expr>, cx: &Cx<'_>| -> Option<Option<String>> {
            match e {
                None => Some(None),
                Some(e) => match int(e) {
                    Some(-1) => Some(None),
                    Some(n) => Some(Some(n.to_string())),
                    None => match e {
                        Expr::Constant(k)
                            if matches!(k.value, rustpython_parser::ast::Constant::None) =>
                        {
                            Some(None)
                        }
                        e => cx.string(e).map(Some),
                    },
                },
            }
        };
        let user = part(arg(c, 1, &["uid", "user"]), &self.cx);
        let group = part(arg(c, 2, &["gid", "group"]), &self.cx);
        let (Some(user), Some(group)) = (user, group) else {
            self.undetermined(format!("`{q}`'s owner is not a literal"));
            return;
        };
        let owner = match (user, group) {
            (None, None) => return,
            (Some(u), None) => u,
            (None, Some(g)) => format!(":{g}"),
            (Some(u), Some(g)) => format!("{u}:{g}"),
        };
        self.one_path(q, c, |path| FsOp::Chown { path, owner });
    }

    /// A module call that is not modelled by name.
    fn other_module_call(&mut self, q: &str, c: &ExprCall) {
        if matches_table(q, READ_ONLY) {
            return;
        }
        if TEMPFILE_CREATORS.contains(&q) {
            if has_keyword(c, "dir") {
                self.undetermined(format!("`{q}` is given `dir=`"));
            }
            return;
        }
        if q == "logging.basicConfig" {
            if has_keyword(c, "filename") {
                self.undetermined("`logging.basicConfig` is given `filename=`");
            }
            return;
        }
        let name = last(q);
        if SAFE_MODULES.contains(&top(q)) {
            self.method_name_only(q, name, c);
            return;
        }
        if !STDLIB.contains(&top(q)) {
            // A third-party module: its import is already Undetermined; a
            // call into it is read by name only.
            self.method_name_only(q, name, c);
            return;
        }
        self.undetermined(format!(
            "`{q}` is not modelled, and its module can run programs or touch the file system"
        ));
    }

    /// A call judged by its method / function name alone.
    fn method_name_only(&mut self, shown: &str, name: &str, c: &ExprCall) {
        if EXEC_NAMES.contains(&name) || FS_METHOD_NAMES.contains(&name) {
            self.undetermined(format!(
                "`{shown}` may run a program or change the file system, and it is not modelled"
            ));
        } else if FILE_WRITER_NAMES.contains(&name) && !(c.args.is_empty() && c.keywords.is_empty())
        {
            self.undetermined(format!("`{shown}` may write a file named by its argument"));
        } else if name == "open" {
            let mode = c.keywords.iter().find_map(|k| {
                k.arg
                    .as_ref()
                    .filter(|a| a.as_str() == "mode")
                    .map(|_| &k.value)
            });
            let literal_modes = c.args.iter().filter_map(|a| self.cx.string(a));
            let writes = literal_modes
                .chain(mode.and_then(|m| self.cx.string(m)))
                .any(|m| m.len() <= 3 && mode_effect(&m, String::new()).is_some());
            if writes || mode.is_some_and(|m| self.cx.string(m).is_none()) {
                self.undetermined(format!("`{shown}` may open a file for writing"));
            }
        } else if name == "replace" && c.args.len() == 1 && c.keywords.is_empty() {
            self.undetermined(format!(
                "`{shown}` with one argument may be `pathlib`'s rename"
            ));
        }
    }

    /// A method call on a receiver that is not an import.
    fn method_call(&mut self, attr: &str, recv: &Expr, c: &ExprCall) {
        if !self.is_path_obj(recv, 0) {
            self.method_name_only(&format!(".{attr}"), attr, c);
            return;
        }
        let call = format!("pathlib.Path.{attr}");
        let call = call.as_str();
        let needs_path = matches!(
            attr,
            "write_text"
                | "write_bytes"
                | "unlink"
                | "rmdir"
                | "rename"
                | "replace"
                | "touch"
                | "mkdir"
                | "chmod"
                | "lchmod"
                | "symlink_to"
                | "hardlink_to"
                | "open"
        );
        if !needs_path {
            self.method_name_only(call, attr, c);
            return;
        }
        if splatted(c) {
            self.undetermined(format!("`{call}` is called with `*args` / `**kwargs`"));
            return;
        }
        if attr == "open" {
            let path = recv.clone();
            self.open_like(call, c, Some(&path), arg(c, 0, &["mode"]));
            return;
        }
        let Some(p) = self.path_operand(call, Some(recv)) else {
            return;
        };
        let op = match attr {
            "write_text" | "write_bytes" => FsOp::Truncate { path: p },
            "unlink" => FsOp::Delete {
                path: p,
                recursive: false,
            },
            "rmdir" => FsOp::RemoveDir { path: p },
            "touch" => FsOp::Touch { path: p },
            "mkdir" => FsOp::MakeDir { path: p },
            "rename" | "replace" => match self.path_operand(call, arg(c, 0, &["target"])) {
                Some(to) => FsOp::Move { from: p, to },
                None => return,
            },
            "symlink_to" | "hardlink_to" => match self.path_operand(call, arg(c, 0, &["target"])) {
                Some(target) => FsOp::Link {
                    target,
                    link: p,
                    symbolic: attr == "symlink_to",
                },
                None => return,
            },
            // chmod / lchmod
            _ => match arg(c, 0, &["mode"])
                .and_then(int)
                .and_then(|m| u32::try_from(m).ok())
            {
                Some(mode) => FsOp::Chmod { path: p, mode },
                None => {
                    self.undetermined(format!("`{call}`'s mode is not an integer literal"));
                    return;
                }
            },
        };
        self.push(call, op);
    }
}

impl Visit for Judge<'_> {
    fn bind(&mut self, name: &str, how: Bind<'_>) {
        if let Bind::Import(q) = how {
            if !STDLIB.contains(&top(&q)) {
                self.undetermined(format!(
                    "`import {q}` loads code that is not in the command (only standard-library \
                     modules are read; `{name}` is not one)",
                ));
            }
        }
    }

    fn fail(&mut self, why: String) {
        self.undetermined(why);
    }

    fn discarded(&mut self, e: &Expr) {
        self.inert_result(e);
    }

    fn call(&mut self, c: &ExprCall) {
        // Before the callee and arguments are walked (the walker calls this
        // hook first), so `name_head` sees the inert marks.
        self.display_args(c);
        match c.func.as_ref() {
            Expr::Name(n) => match self.cx.b.resolve(n.id.as_str()) {
                Resolved::Module(q) => {
                    let q = q.to_string();
                    self.module_call(&q, c);
                }
                Resolved::Unbound if n.id.as_str() == "open" => {
                    if splatted(c) {
                        self.undetermined("`open` is called with `*args` / `**kwargs`");
                        return;
                    }
                    self.open_like("open", c, arg(c, 0, &["file"]), arg(c, 1, &["mode"]));
                }
                // Another builtin, or a function / value of the program
                // (whose body is walked on its own).
                Resolved::Unbound | Resolved::Value(_) | Resolved::Ambiguous { .. } => {}
            },
            Expr::Attribute(a) => match self.cx.qual(&c.func) {
                Some(q) => self.module_call(&q, c),
                None => self.method_call(a.attr.as_str(), &a.value, c),
            },
            // A computed callee (`f()()`, `fs[0](x)`): whatever it calls was
            // reached as a value somewhere, which is judged there.
            _ => {}
        }
    }

    fn name_value(&mut self, n: &ExprName) {
        let id = n.id.as_str();
        self.name_head(n);
        if id == "open" && matches!(self.cx.b.resolve(id), Resolved::Unbound) {
            self.undetermined("the builtin `open` is used as a value");
            return;
        }
        if let Resolved::Module(q) = self.cx.b.resolve(id) {
            if !inert_value(q) {
                let q = q.to_string();
                self.undetermined(format!(
                    "`{id}` (`{q}`) is used as a value, so what it is called with cannot be \
                     followed"
                ));
            }
        }
    }

    fn name_head(&mut self, n: &ExprName) {
        let id = n.id.as_str();
        if (NAMESPACE_BUILTINS.contains(&id) || id == "compile") && self.inert_ns.contains(&n.range)
        {
            // Inert (see `inert_result` / `display_args`): the call's result,
            // or the bare builtin itself, is only displayed or discarded, so
            // nothing fetched from the namespace is called and no compiled
            // code is run.
        } else if DANGEROUS_BUILTINS.contains(&id) {
            self.undetermined(format!(
                "the program uses `{id}`, which runs code or reaches objects by a computed name"
            ));
        } else if is_dunder(id) && !HARMLESS_DUNDERS.contains(&id) {
            self.undetermined(format!("the program uses `{id}`"));
        } else if matches!(
            self.cx.b.resolve(id),
            Resolved::Ambiguous { imported: true }
        ) {
            self.undetermined(format!(
                "`{id}` is bound by an import and rebound, so its calls cannot be followed"
            ));
        }
    }

    fn attr_value(&mut self, a: &ExprAttribute) {
        self.attr_head(a);
        match a.ctx {
            ExprContext::Store | ExprContext::Del => {
                if let Some(q) = self.cx.qual(&a.value) {
                    self.undetermined(format!("the program assigns into the module `{q}`"));
                }
            }
            ExprContext::Load => match self.cx.qual(&Expr::Attribute(a.clone())) {
                Some(q) => {
                    if !inert_value(&q) {
                        self.undetermined(format!(
                            "`{q}` is used as a value, so what it is called with cannot be \
                             followed"
                        ));
                    }
                }
                None => {
                    let name = a.attr.as_str();
                    if EXEC_NAMES.contains(&name)
                        || FS_METHOD_NAMES.contains(&name)
                        || FILE_WRITER_NAMES.contains(&name)
                    {
                        self.undetermined(format!(
                            "`.{name}` is used as a value, so what it is called with cannot be \
                             followed"
                        ));
                    }
                }
            },
        }
    }

    fn attr_head(&mut self, a: &ExprAttribute) {
        let name = a.attr.as_str();
        if is_dunder(name) && !HARMLESS_DUNDERS.contains(&name) {
            self.undetermined(format!(
                "the program reads the attribute `{name}`, which reaches another namespace"
            ));
        }
        if self.cx.qual(&Expr::Attribute(a.clone())).as_deref() == Some("sys.modules") {
            self.undetermined("the program reads `sys.modules`, which reaches any module by name");
        }
    }

    fn subscript_store(&mut self, s: &ExprSubscript) {
        if let Some(q) = self.cx.qual(&s.value) {
            self.undetermined(format!("the program assigns into `{q}`"));
        }
    }
}

#[cfg(test)]
mod printed_namespace_tests {
    use crate::python::analyze;
    use crate::Analysis;

    fn known_empty(src: &str) -> bool {
        matches!(analyze(src, &[]), Analysis::Known(e) if e.is_empty())
    }

    /// Backlog 20261115: the bare builtin, or its no-arg call, only printed.
    #[test]
    fn bare_or_called_namespace_builtin_printed_is_inert() {
        for src in [
            "print(vars)",
            "print(locals())",
            "print(globals())",
            "print(1, vars, 2)",
        ] {
            assert!(known_empty(src), "{src}");
        }
    }

    #[test]
    fn other_uses_of_a_namespace_builtin_stay_undetermined() {
        for src in [
            "print(vars()[\"exec\"](input()))",
            "print(*vars)",
            "print(sep=vars)",
            "print((vars, 1))",
            "print([globals])",
            "print(vars.x)",
            "g = vars\nprint(g)",
            "print = repr\nprint(vars)",
            "vars = 1\nprint(vars)",
            "f = print\nf(vars)",
            "repr(vars)",
            "vars",
            "getattr(__builtins__, \"exec\")(input())",
            "__builtins__.exec(input())",
            "globals()[\"__builtins__\"].exec(input())",
            "f=print; g=globals; f(g()[\"exec\"](input()))",
            "print(getattr)",
            "print(__builtins__)",
        ] {
            assert!(
                matches!(analyze(src, &[]), Analysis::Undetermined(_)),
                "{src}"
            );
        }
    }
}
