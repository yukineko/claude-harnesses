- `check --schema <name>`: validate a JSON value read from `--file <path>` or stdin against a named schema [test: crates/schemaguard/tests/prose_claims.rs::claim_check_file_input].
  When the input parses, it prints `{valid, schema, errors[], undetermined[], not_checked[]}` (`undetermined` is omitted on a clean verdict) [test: crates/schemaguard/tests/prose_claims.rs::claim_check_output_keys].
  Exit `0` valid, `1` schema violations, `2` parse error / unknown schema / a declared check could not be applied and no violation was found [test: crates/schemaguard/tests/prose_claims.rs::claim_check_exit_codes] [test: crates/schemaguard/tests/prose_claims.rs::claim_undetermined_exit_codes].
- `metrics`: print reject counts per schema from `~/.schemaguard/rejects.jsonl` (`--json` for machine-readable) [test: crates/schemaguard/tests/prose_claims.rs::claim_metrics_reads_store].
  A count is not the number of rejected checks: one `check` with two field violations adds `2`, a parse failure adds `1` [test: crates/schemaguard/tests/prose_claims.rs::claim_metrics_sums_violations].
  Exit `0` when the store is absent or readable; exit `2` (stdout `{"status":"unknown",...}` with `--json`) when the store exists but cannot be read or has a malformed line [test: crates/schemaguard/tests/prose_claims.rs::claim_metrics_exit].
- `list`: List known schema names; exit `0` [test: crates/schemaguard/tests/integration.rs::list_prints_known_schema_names].

Once the schema name resolves, `check` prints a single JSON verdict object on stdout, including for unreadable or unparseable input [test: crates/schemaguard/tests/prose_claims.rs::claim_check_single_json_stdout].
An unknown schema, or a missing `--schema`, prints nothing on stdout and exits `2` [test: crates/schemaguard/tests/prose_claims.rs::claim_check_single_json_stdout].

- **Valid, field-invalid, or undetermined** (JSON parsed, schema resolved): [test: crates/schemaguard/tests/prose_claims.rs::claim_check_output_keys]

  ```json
  {
    "valid": true|false,
    "schema": "<name>",
    "errors": [ { "path": "a.b", "problem": "..." } ],
    "undetermined": [ { "path": "a.b", "problem": "..." } ],
    "not_checked": [ { "path": "a.b", "reason": "..." } ]
  }
  ```

  `errors[]` is empty when `valid` is `true`, and `undetermined` is omitted on that arm [test: crates/schemaguard/tests/prose_claims.rs::claim_clean_arm_shape].
  When `valid` is `false` each `errors[]` entry is `{path, problem}` — a path into the payload such as `tasks[0].id` plus a non-empty problem text [test: crates/schemaguard/tests/prose_claims.rs::claim_errors_entry_path_problem].

  - `errors[]` — the check **ran and failed** [test: crates/schemaguard/tests/undetermined_probe.rs::missing_required_name_exits_one].
  - `undetermined[]` — a declared constraint the engine **could not apply** to the value it was handed (for example an `items` sub-schema against a non-array) [test: crates/schemaguard/tests/prose_claims.rs::claim_undetermined_exit_codes].
  - "Could not check" is not "passed": a verdict with an `undetermined[]` entry never exits `0` [test: crates/schemaguard/tests/prose_claims.rs::claim_undetermined_exit_codes].
    It exits `2` when `errors[]` is empty and `1` when `errors[]` is non-empty too [test: crates/schemaguard/tests/prose_claims.rs::claim_undetermined_exit_codes].
  - `not_checked[]` — checks **deliberately not performed**, each entry `{path, reason}` with a non-empty reason: [test: crates/schemaguard/tests/prose_claims.rs::claim_not_checked_entry_reason]
    - an absent `required: false` field (its type/enum/items constraints were never evaluated), [test: crates/schemaguard/tests/integration.rs::valid_verdict_names_the_declared_checks_it_did_not_perform]
    - a `Ty::Any` field (no type check), [test: crates/schemaguard/src/schema.rs::any_type_is_waived_not_undetermined]
    - an array with no `items` schema, [test: crates/schemaguard/tests/prose_claims.rs::claim_array_without_items_not_checked]
    - and any key the schema does not declare (unknown extra fields are allowed) [test: crates/schemaguard/src/schema.rs::waived_checks_do_not_block].
  - `not_checked[]` entries do **not** block [test: crates/schemaguard/src/schema.rs::waived_checks_do_not_block].

- **Input failure** (unreadable stdin/file or unparseable JSON): the verdict is [test: crates/schemaguard/tests/prose_claims.rs::claim_input_failure_shape]

  ```json
  { "valid": false, "error": "<message>" }
  ```

  Note the singular `error` (a string) rather than the plural `errors` array [test: crates/schemaguard/tests/prose_claims.rs::claim_input_failure_shape].

An **unknown schema** is reported earlier, before any input is read: a plain message goes to stderr (not a JSON verdict on stdout) and the process exits `2` [test: crates/schemaguard/tests/prose_claims.rs::claim_unknown_schema_stderr].

Parse failures, field violations and undetermined checks are all counted as rejects in `metrics`; a clean check is not [test: crates/schemaguard/tests/prose_claims.rs::claim_rejects_counted].

Declared schemas: `decomposition`, `episode`, `playbook`, `scout-measure`, `verdict`, and `undetermined-probe` (see `schemaguard list`) [test: crates/schemaguard/tests/prose_claims.rs::claim_list_exact_names].

The plugin bundles the `bin/schemaguard` launcher and declares no lifecycle hooks [test: crates/schemaguard/tests/prose_claims.rs::claim_plugin_no_hooks].

Invoke `schemaguard check --schema <name>` at a handoff and act on the exit code (re-ask on `1`/`2`) [test: crates/schemaguard/tests/prose_claims.rs::claim_check_exit_codes].

```sh
cargo install --path .
schemaguard list
echo '{...}' | schemaguard check --schema decomposition   # validate stdin [test: crates/schemaguard/tests/integration.rs::check_valid_decomposition_exits_zero]
schemaguard check --schema episode --file out.json
schemaguard metrics --json
```

```sh
cargo test
```

Only the `bin/schemaguard` launcher is committed; the per-platform `bin/schemaguard-darwin-*` and `bin/schemaguard-linux-*` binaries are git-ignored, while the `bin/schemaguard-windows-*.exe` names are not [test: crates/schemaguard/tests/prose_claims.rs::claim_launcher_only_committed].
They are built and copied by `scripts/rebuild-plugins.sh`, which `scripts/rollout-plugins.sh` runs [test: crates/schemaguard/tests/prose_claims.rs::claim_launcher_only_committed].
