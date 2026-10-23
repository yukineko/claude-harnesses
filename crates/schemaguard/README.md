- `check --schema <name>`: validate a JSON value read from `--file <path>` or stdin against a named schema.
  When the input parses, it prints `{valid, schema, errors[], undetermined[], not_checked[]}` (`undetermined` is omitted on a clean verdict).
  Exit `0` valid, `1` schema violations, `2` parse error / unknown schema / a declared check could not be applied and no violation was found.
- `metrics`: print reject counts per schema from `~/.schemaguard/rejects.jsonl` (`--json` for machine-readable).
  A count is not the number of rejected checks: one `check` with two field violations adds `2`, a parse failure adds `1`.
  Exit `0` when the store is absent or readable; exit `2` (stdout `{"status":"unknown",...}` with `--json`) when the store exists but cannot be read or has a malformed line.
- `list`: List known schema names; exit `0`.

Once the schema name resolves, `check` prints a single JSON verdict object on stdout, including for unreadable or unparseable input.
An unknown schema, or a missing `--schema`, prints nothing on stdout and exits `2`.

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

  `errors[]` is empty when `valid` is `true`, and `undetermined` is omitted on that arm.
  When `valid` is `false` each `errors[]` entry is `{path, problem}` — a path into the payload such as `tasks[0].id` plus a non-empty problem text.

  - `errors[]` — the check **ran and failed**.
  - `undetermined[]` — a declared constraint the engine **could not apply** to the value it was handed (for example an `items` sub-schema against a non-array).
  - "Could not check" is not "passed": a verdict with an `undetermined[]` entry never exits `0`.
    It exits `2` when `errors[]` is empty and `1` when `errors[]` is non-empty too.
  - `not_checked[]` — checks **deliberately not performed**, each entry `{path, reason}` with a non-empty reason:
    - an absent `required: false` field (its type/enum/items constraints were never evaluated),
    - a `Ty::Any` field (no type check),
    - an array with no `items` schema,
    - and any key the schema does not declare (unknown extra fields are allowed).
  - `not_checked[]` entries do **not** block.

- **Input failure** (unreadable stdin/file or unparseable JSON): the verdict is

  ```json
  { "valid": false, "error": "<message>" }
  ```

  Note the singular `error` (a string) rather than the plural `errors` array.

An **unknown schema** is reported earlier, before any input is read: a plain message goes to stderr (not a JSON verdict on stdout) and the process exits `2`.

Parse failures, field violations and undetermined checks are all counted as rejects in `metrics`; a clean check is not.

Declared schemas: `decomposition`, `episode`, `playbook`, `scout-measure`, `verdict`, and `undetermined-probe` (see `schemaguard list`).

The plugin bundles the `bin/schemaguard` launcher and declares no lifecycle hooks.

Invoke `schemaguard check --schema <name>` at a handoff and act on the exit code (re-ask on `1`/`2`).

```sh
cargo install --path .
schemaguard list
echo '{...}' | schemaguard check --schema decomposition   # validate stdin
schemaguard check --schema episode --file out.json
schemaguard metrics --json
```

```sh
cargo test
```

Only the `bin/schemaguard` launcher is committed; the per-platform `bin/schemaguard-darwin-*` and `bin/schemaguard-linux-*` binaries are git-ignored, while the `bin/schemaguard-windows-*.exe` names are not.
They are built and copied by `scripts/rebuild-plugins.sh`, which `scripts/rollout-plugins.sh` runs.
