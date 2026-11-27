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
/// command anywhere could make a name readonly ([`LocalTrust::note_command`]);
/// otherwise [`run_passes`] evaluates again with the trust off, which is the
/// old (restrictive) behaviour. A readonly inherited from the surrounding
/// session is not seen (the top level trusts its own assignments alike).
#[derive(Clone, Copy)]
pub(super) struct LocalTrust {
    pub(super) enabled: bool,
    /// A trusted `local` bound a value in this pass.
    pub(super) used: bool,
    /// Some command may make a variable readonly or change variables unseen.
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

    /// Record `sc` when it could make a variable readonly (superset: any
    /// command word not known, any builtin that evals / declares).
    pub(super) fn note_command(&mut self, sc: &SimpleCommand) {
        let risky = match sc.argv.first().map(|w| &w.value) {
            None => false,
            Some(Value::Undetermined(_)) => true,
            Some(Value::Literal(n)) => match n.as_str() {
                "readonly" | "declare" | "typeset" | "eval" | "source" | "." | "builtin"
                | "command" | "trap" | "alias" | "unalias" | "enable" | "shopt" | "exec" => true,
                "local" | "export" => sc.argv.iter().skip(1).any(|w| match &w.value {
                    Value::Undetermined(_) => true,
                    Value::Literal(a) => a.starts_with(['-', '+']),
                }),
                _ => false,
            },
        };
        self.risk |= risky;
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
