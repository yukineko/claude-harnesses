# progeffect

The EFFECTS of an embedded program — the shell commands it runs, the
processes it starts, the file-system operations it performs on literal paths —
read with a real parser for its language. A pure library (no I/O); not a
distributed plugin (no `.claude-plugin/plugin.json`). Linked into blastguard
(stage S5, inline Python programs) and meant for the future script-quarantine
plugin.

User rulings (2026-11-17): embedded Python gets a real parser and its effects
are judged with the same effect × target rules as shell; the per-language
parsers and the effect × target rules live in ONE shared lib crate; text vs
code is decided by a real parser, never by heuristics. Versioned like
harness-core: only `Cargo.toml`'s version, bumped on every linked change
(`scripts/check-version-bumped.py` `SHARED_CRATES`). Spec:
`docs/specs/blastguard.md` ("inline Python program の parser 判定（S5）") and
`crates/blastguard/src/detect/ast/SUBSET.md` ("Python programs (S5)").

## API

```rust
let argv = [Some("-c".to_string())];
match progeffect::python::analyze("import os; os.remove('/tmp/x')", &argv) {
    progeffect::Analysis::Known(effects) => {
        for e in &effects {
            // `rm -- '/tmp/x'`: judge it with the host's shell rules.
            let _ = e.shell_spelling();
        }
    }
    progeffect::Analysis::Undetermined(why) => { /* restrictive side: Ask / Deny */ }
}
```

* `Analysis::Known(Vec<Effect>)` — every call of the program that can run a
  process or touch the file system (reachable or not), each with literal
  operands. Empty = no effect.
* `Analysis::Undetermined(String)` — the program does something whose effect
  cannot be read from its text. Never "no effect": a consumer resolves it to
  its restrictive side.
* `Effect::shell_spelling()` — the shell command with exactly that effect on
  exactly those operands (`: > 'P'`, `rm -rf -- 'P'`, `mv -- 'A' 'B'`, …). The
  crate does NOT place paths (system / project / scratch): the host judges the
  spelling with its own rules, so there is one rule table.

## Dependency

`rustpython-parser = { version = "0.4", default-features = false, features =
["num-bigint"] }`. The default features pull malachite (LGPL), which must not
be linked:

```
cargo tree -p progeffect -e normal | grep -i malachite   # prints nothing
```

## What the Python reading follows, and what is Undetermined

See the module doc of `src/python.rs` and the tables in
`src/python/tables.rs` (every table is a closed list with its restrictive
default stated). In short: string literals, comments and f-string literal
parts are data; calls are effects; `exec` / `eval` / `getattr` / `globals` /
`__dict__` / `__import__` / `importlib` / non-standard-library imports /
aliases that cannot be followed / non-literal operands / parse errors are
Undetermined.

## Tests

Written by independent authors (CLAUDE.md §2(a)); the acceptance tests live in
`crates/blastguard/tests/` and exercise this crate through the blastguard hook.
