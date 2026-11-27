//! Function definitions of `detect::ast::eval` (`f() { …; }`, `function f { …; }`),
//! evaluated only under `evaluate_with_function_definitions`. A child module of
//! `eval.rs`, so it reads the evaluator's state (`Env`, `Ev`, `Text`) directly.
//!
//! The body runs when the function is called, not here, so it is evaluated
//! once with an opaque state: every variable, the cwd and the stdin are
//! `Undetermined` (late-bound). Its commands are emitted for judgment. A
//! later call can change any variable, so the state after the definition is
//! opaque too. Any unsupported construct in the body makes the whole analysis
//! `Undetermined(Unsupported)`. A literal assignment in the body may therefore
//! come out `Unsupported`, and `HOME` in the body is `Undetermined`; both are
//! fail-closed.

use super::super::{Cause, Context, Script, SimpleCommand, Value};
use super::{Env, Ev, Mode, Origin, Text};
use brush_parser::ast;

/// Whether a static `local NAME=literal` inside a function body binds `NAME`
/// even though the body runs in an opaque state.
///
/// Both bash and zsh bind it (measured 2026-11-21 / 2026-11-27, bash 3.2.57,
/// zsh 5.9), and neither lets a global `declare -i` leak into the `local`. The
/// one exception measured: a GLOBALLY READONLY name makes bash's `local` fail
/// (`local: P: readonly variable`, the old value stays). A body is evaluated
/// once, when it is defined, so a readonly set earlier or later on the line is
/// invisible to it. The trust is therefore only kept for a script in which no
/// command anywhere is outside the allowlist of [`LocalTrust::note_command`];
/// otherwise [`run_passes`] evaluates again with the trust off, which is the
/// old (restrictive) behaviour. A readonly inherited from the surrounding
/// session is not seen (the top level trusts its own assignments alike).
#[derive(Clone, Copy)]
pub(super) struct LocalTrust {
    pub(super) enabled: bool,
    /// A trusted `local` bound a value in this pass.
    pub(super) used: bool,
    /// Some command is not on the allowlist (it may make a name readonly or
    /// write a variable the evaluator does not model).
    risk: bool,
    /// Function definitions evaluated (any pass, nested ones too). With two
    /// or more, a call from a body may run another function that rewrites the
    /// body's `local` through dynamic scope, so the trust is not kept.
    defs: usize,
}

impl LocalTrust {
    pub(super) fn new(enabled: bool) -> Self {
        LocalTrust {
            enabled,
            used: false,
            risk: false,
            defs: 0,
        }
    }

    /// Record `sc` as a risk unless it is on the ALLOWLIST: an assignment-only
    /// command, or a command word that is known not to write a variable the
    /// evaluator does not model. Everything else (an undetermined word, a user
    /// function, `readonly` / `declare` / `eval` / `read` / `print` /
    /// `zformat` / `vared` / …, any unlisted word) drops the trust.
    pub(super) fn note_command(&mut self, sc: &SimpleCommand, functions: &[String]) {
        /// Builtins whose variable effects `builtin_effects` models exactly
        /// (they write no unmodelled name), and external programs, which
        /// cannot write a shell variable.
        const ALLOWED: &[&str] = &[
            ":", "true", "false", "echo", "test", "[", "pwd", "cd", "return", "exit", "break",
            "continue", "unset", "printf", "local", "export", "cat", "ls", "rm", "mkdir", "rmdir",
            "touch", "cp", "mv", "ln", "chmod", "grep", "sed", "awk", "tee", "head", "tail", "wc",
            "sort", "uniq", "cut", "tr", "find", "xargs", "git", "cargo", "make", "date", "sleep",
            "curl", "diff", "basename", "dirname",
        ];
        let risky = match sc.argv.first().map(|w| &w.value) {
            None => false,
            Some(Value::Undetermined(_)) => true,
            // A call of a function defined on this line: its body was
            // evaluated (and its commands noted) at the definition, and
            // `defs > 1` covers another function rewriting a `local`.
            Some(Value::Literal(n)) if functions.contains(n) => false,
            Some(Value::Literal(n)) if !ALLOWED.contains(&n.as_str()) => true,
            // Listed, but an option may name a variable (`printf -v`) or
            // change an attribute (`local -r`, `export -n`).
            Some(Value::Literal(n)) => {
                matches!(n.as_str(), "printf" | "local" | "export" | "unset")
                    && sc.argv.iter().skip(1).any(|w| match &w.value {
                        Value::Undetermined(_) => n != "printf",
                        Value::Literal(a) => a.starts_with(['-', '+']),
                    })
            }
        };
        // `{NAME}>file` / `{NAME}<f` / `{NAME}>&-` (zsh, bash) assigns an fd
        // number to NAME; the parser leaves `{NAME}` as an ordinary word, so
        // any such word on ANY command (listed or not) drops the trust.
        let fd_var = sc.argv.iter().any(|w| match &w.value {
            Value::Literal(s) => {
                s.len() > 2
                    && s.starts_with('{')
                    && s.ends_with('}')
                    && s[1..]
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            }
            Value::Undetermined(_) => false,
        });
        self.risk |= risky || fd_var;
    }
}

/// Evaluate the program with `local` trusted; when that trust was used and a
/// command that may make a name readonly exists, evaluate again without it.
pub(super) fn run_passes(
    command: &str,
    program: &ast::Program,
    function_defs: bool,
) -> Result<Script, Cause> {
    let (script, trust) = run_once(command, program, function_defs, true)?;
    if trust.used && (trust.risk || trust.defs > 1) {
        return run_once(command, program, function_defs, false).map(|(s, _)| s);
    }
    Ok(script)
}

fn run_once(
    command: &str,
    program: &ast::Program,
    function_defs: bool,
    trust: bool,
) -> Result<(Script, LocalTrust), Cause> {
    let text = Text::new(command, Origin::root(command.len()));
    let mut ev = Ev {
        out: Vec::new(),
        emit: true,
        work: 0,
        accums: Vec::new(),
        function_defs,
        function_names: Vec::new(),
        has_function_definition: false,
        function_body_ranges: Vec::new(),
        function_depth: 0,
        local_trust: LocalTrust::new(trust),
    };
    ev.program(&text, program, Env::initial(), &Context::TopLevel)?;
    let script = Script {
        commands: ev.out,
        function_names: ev.function_names,
        has_function_definition: ev.has_function_definition,
        function_body_ranges: ev.function_body_ranges,
    };
    Ok((script, ev.local_trust))
}

impl Ev {
    pub(super) fn function_def(
        &mut self,
        text: &Text,
        f: &ast::FunctionDefinition,
        mut env: Env,
        ctx: &Context,
    ) -> Result<Env, Cause> {
        self.local_trust.defs += 1;
        // Redirections on the body apply at each call: not modelled.
        if f.body.1.as_ref().is_some_and(|r| !r.0.is_empty()) {
            return Err(Cause::Unsupported(
                "function definition with redirections: not evaluated".into(),
            ));
        }
        // The name is read from a scratch copy: only a literal name is accepted.
        let mut scratch = env.clone();
        let name = match self.word(text, &f.fname, Mode::Assign, &mut scratch)?.value {
            Value::Literal(s) => s,
            Value::Undetermined(r) => {
                return Err(Cause::Unsupported(format!(
                    "function name undetermined ({r}): not evaluated"
                )))
            }
        };
        // The body, once, in a state where nothing is known.
        let mut body = Env::initial();
        body.make_opaque("function body: runs when the function is called");
        let first = self.out.len();
        self.function_depth += 1;
        let body_result = self.compound(text, &f.body.0, body, ctx);
        self.function_depth -= 1;
        body_result?;
        if self.emit {
            self.function_body_ranges.push(first..self.out.len());
            self.has_function_definition = true;
            if !self.function_names.contains(&name) {
                self.function_names.push(name);
            }
        }
        // A later call may change any variable, the cwd or the stdin.
        env.make_opaque("a function definition: a later call may change any variable");
        Ok(env)
    }
}

impl Env {
    /// An option word with an attached value (`-aP`, `-vP`): every suffix of
    /// it that is a name may be what the builtin assigns (superset).
    pub(super) fn forget_attached(&mut self, word: &str, why: &str) -> Result<(), Cause> {
        if word.starts_with('-') {
            for (i, _) in word.char_indices().skip(1) {
                if super::is_ident(&word[i..]) {
                    self.int_assign(&word[i..], &super::undet(why.to_string()))?;
                    self.forget_operand(&word[i..], why)?;
                }
            }
        }
        Ok(())
    }
}

impl Env {
    /// Structural fail-closed rule (backlog f3301770): after a simple command,
    /// every tracked variable whose name appears in one of its words other than
    /// as a plain `$name` reference is `Undetermined`. A list of known
    /// binders (`read`, `printf -v`, zsh's `print -v`, `zformat`, `zstat -A`,
    /// `sysread`, `vared`, `zparseopts`, `{name}` words, `name[i]=`, behind
    /// `command` / `builtin`) cannot be complete, so an unrecognised command
    /// naming the variable counts too. A prefix assignment to one of zsh's
    /// tied names (`PATH` / `path`, ...) rebinds its partner. Costs a
    /// resolution, never invents one. `None`: nothing was produced.
    pub(super) fn forgetting_in(mut self, sc: Option<&SimpleCommand>) -> Self {
        use crate::detect::vars;
        let Some(sc) = sc else { return self };
        // The RAW spelling: an unquoted `{P[0]}` is a glob, hence
        // Undetermined, yet it is still a redirect word naming `P`.
        let words: Vec<&str> = sc.argv.iter().map(|w| w.raw.as_str()).collect();
        let undetermined_operand = sc
            .argv
            .iter()
            .any(|w| matches!(w.value, Value::Undetermined(_)));
        // An undetermined word may be the NAME a binder assigns
        // (`print -v $n`): for a command known to bind a name operand, no
        // tracked variable can be trusted afterwards. (An unknown command
        // with an undetermined word naming a variable is not covered: the
        // name cannot be read from the text.)
        if (undetermined_operand
            && words
                .first()
                .is_some_and(|h| vars::binds_name_as_operand(h)))
            || vars::indirect_name_operand(&words)
        {
            self.havoc_vars("a name operand of a command that binds names is an expansion");
        }
        // A command that may redefine what a command word means (zsh
        // `autoload`, `functions -c`, `functions[f]=`) makes the state opaque,
        // like `alias` / `shopt` / `eval` do: the non-binder exemption is
        // then off (`forget_mentioned`).
        let head = words
            .first()
            .map(|h| crate::detect::vars::normalized_head(h));
        if matches!(head.as_deref(), Some("autoload" | "functions"))
            || sc
                .assignments
                .iter()
                .any(|(n, _)| n.starts_with("functions["))
        {
            self.make_opaque("a command that may redefine functions");
        }
        self.forget_mentioned(&words);
        // zsh `typeset -T A B`: remember the tie (the key is not an
        // identifier, so it never collides with a variable).
        if let Some((a, b)) = vars::tie_pair(&words) {
            self.forget(&a, "tied by typeset -T");
            self.forget(&b, "tied by typeset -T");
            self.vars.insert(
                format!("\u{1}tie\u{1}{a}\u{1}{b}"),
                super::undet("a `typeset -T` tie"),
            );
        }
        for (n, _) in &sc.assignments {
            if let Some(p) = vars::tied_partner(n) {
                self.forget(p, "tied to an assigned name");
            }
            let ties: Vec<(String, String)> = self
                .vars
                .keys()
                .filter_map(|k| {
                    let mut parts = k.strip_prefix("\u{1}tie\u{1}")?.split('\u{1}');
                    Some((parts.next()?.to_string(), parts.next()?.to_string()))
                })
                .collect();
            for (a, b) in ties {
                if a == *n {
                    self.forget(&b, "tied to an assigned name");
                } else if b == *n {
                    self.forget(&a, "tied to an assigned name");
                }
            }
        }
        self
    }

    /// Forget every tracked variable some word of `words` names other than as
    /// a plain `$name` reference; a bare `name` operand of a command that
    /// cannot bind (`echo P`, `grep P f`) is data and keeps it.
    fn forget_mentioned(&mut self, words: &[&str]) {
        use crate::detect::vars;
        let names: Vec<String> = self
            .vars
            .keys()
            .filter(|k| !k.starts_with('\u{1}'))
            .cloned()
            .collect();
        for k in names {
            if words.iter().any(|w| {
                vars::mentions_name_outside_reference(w, &k)
                    && (self.opaque || !vars::exempt_plain_arg(words, w, &k))
            }) {
                self.forget(&k, "named by a command word that may bind it");
            }
        }
    }

    /// [`Env::forgetting_in`] for one stage of a multi-stage pipeline: zsh
    /// runs the last stage in the parent shell, so a compound stage
    /// (`while read P; …`, `{ read P; }`) is walked through its source text.
    pub(super) fn forgetting_stage(
        mut self,
        text: &Text,
        c: &ast::Command,
        produced: Option<&SimpleCommand>,
    ) -> Self {
        use crate::detect::vars;
        let ast::Command::Compound(cc, _) = c else {
            return self.forgetting_in(produced);
        };
        let src = super::compound_loc(cc)
            .and_then(|l| text.local(l))
            .and_then(|r| text.s.get(r));
        match src {
            Some(src) => {
                let names: Vec<String> = self
                    .vars
                    .keys()
                    .filter(|k| !k.starts_with('\u{1}'))
                    .cloned()
                    .collect();
                for k in names {
                    if vars::compound_mentions_name(src, &k, !self.opaque) {
                        self.forget(&k, "named inside a pipeline stage that may bind it");
                    }
                }
                if vars::compound_source_has_indirect_binder(src) {
                    self.havoc_vars("a pipeline stage binds a name given by an expansion");
                }
            }
            None => self.havoc_vars("a compound pipeline stage with no source text"),
        }
        self
    }
}

impl Env {
    /// zsh's `[[ x =~ re ]]` sets these (bash sets BASH_REMATCH only).
    pub(super) fn forget_zsh_match(&mut self) {
        for n in ["MATCH", "match", "MBEGIN", "MEND", "mbegin", "mend"] {
            self.forget(n, "set by `=~` in zsh");
        }
    }
}
