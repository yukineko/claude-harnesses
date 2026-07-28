# schemaguard

Schema-validation gate for LLM structured outputs at source→executor
boundaries. When one stage of the harness hands a JSON payload to the next
(a decomposition, an episode record, a playbook, a scout measure), schemaguard
validates it against a *named, declared* schema, emits a structured error so the
producer can re-ask exactly once, and counts every reject to metrics — so a
silently-dropped or malformed payload becomes observable instead of vanishing.

Subscription-native: one bundled Rust binary, **no API key**. It's a plain CLI,
not a lifecycle hook — call it wherever a structured handoff happens and branch
on its exit code.

## What it does

| Subcommand | What it does | Exit codes |
|---|---|---|
| `check --schema <name>` | Validate a JSON value (`--file <path>` or stdin) against a named schema; print `{valid, schema, errors[], not_checked[]}` | `0` valid · `1` schema violations · `2` parse error / unknown schema / a declared check could not be applied |
| `metrics` | Print reject counts per schema (`--json` for machine-readable) | `0` |
| `list` | List known schema names | `0` |

### Verdict schema (`check` output)

`check` always prints a single JSON verdict object on stdout. There are two
shapes, keyed on how far validation got:

- **Valid, field-invalid, or undetermined** (JSON parsed, schema resolved):

  ```json
  {
    "valid": true|false,
    "schema": "<name>",
    "errors": [ { "path": "a.b", "problem": "..." } ],
    "undetermined": [ { "path": "a.b", "problem": "..." } ],
    "not_checked": [ { "path": "a.b", "reason": "..." } ]
  }
  ```

  `errors[]` is empty when `valid` is `true` (and `undetermined` is omitted on
  that arm, since a clean verdict has none by construction). When `valid` is
  `false` each `errors[]` entry is `{path, problem}` — a JSON-path into the
  payload plus a human-readable problem — and this is the re-ask contract the
  producer feeds back to the model.

  The three arrays are the gate's three answers, kept apart on purpose:

  - `errors[]` — the check **ran and failed**.
  - `undetermined[]` — a declared constraint the engine **could not apply** to
    the value it was handed (e.g. an `items` sub-schema against a non-array).
    "Could not check" is not "passed": this exits `2`, never `0`.
  - `not_checked[]` — checks **deliberately not performed**, each with the
    declaration that makes it deliberate: an absent `required: false` field
    (its type/enum/items constraints were never evaluated), a `Ty::Any` field
    (no type check), an array with no `items` schema, and any key the schema
    does not declare (unknown extra fields are allowed). These do **not** block
    — they are printed so that `"valid": true` with `"errors": []` cannot be
    read as "every declared constraint was evaluated and passed" when some were
    never evaluated at all.

- **Input failure** (unreadable stdin/file or unparseable JSON): no per-field
  detail is available, so the verdict is

  ```json
  { "valid": false, "error": "<message>" }
  ```

  Note the singular `error` (a string) rather than the plural `errors` array.

An **unknown schema** is reported earlier, before any input is read: a plain
message goes to stderr (not a JSON verdict on stdout) and the process exits `2`.

Exit codes: `0` (`valid:true`), `1` (`valid:false` with `errors[]`), `2` (input
failure `{valid:false, error}` on stdout, unknown schema on stderr, or
`valid:false` with a non-empty `undetermined[]`). Parse failures, field
violations and undetermined checks are all counted as rejects in `metrics`.

Declared schemas: `decomposition`, `episode`, `playbook`, `scout-measure`
(see `schemaguard list`).

## Install (plugin)

Installed via the plugin marketplace, the bundled `bin/schemaguard` is on hand
for any skill or hook that produces structured output — there are no lifecycle
hooks to wire. Invoke `schemaguard check --schema <name>` at a handoff and act
on the exit code (re-ask on `1`/`2`).

## Standalone (cargo)

```sh
cargo install --path .
schemaguard list                              # show declared schema names
echo '{...}' | schemaguard check --schema decomposition   # validate stdin
schemaguard check --schema episode --file out.json        # validate a file
schemaguard metrics --json                    # reject counts per schema
```

## Build

```sh
cargo test
```

The committed `bin/schemaguard-*` binaries are what the plugin ships, so end
users need neither cargo nor an API key. Rebuild and recommit them (the
workspace builds with `cargo build --workspace --release`) when you change
validation behavior callers rely on.
