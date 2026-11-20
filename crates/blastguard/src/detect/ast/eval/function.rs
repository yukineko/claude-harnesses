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

use super::super::{Cause, Context, Value};
use super::{Env, Ev, Mode, Text};
use brush_parser::ast;

impl Ev {
    pub(super) fn function_def(
        &mut self,
        text: &Text,
        f: &ast::FunctionDefinition,
        mut env: Env,
        ctx: &Context,
    ) -> Result<Env, Cause> {
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
        self.compound(text, &f.body.0, body, ctx)?;
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
