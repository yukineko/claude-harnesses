tracekit records each phase of one run as a **parent-linked span tree**
(`phase, model, ms, cost, status`), renders it as an indented tree, and exports it as
OTLP/JSON [test: crates/tracekit/tests/prose_claims.rs::claim_record_tree_render_export].

## Record a span

Each `tracekit record` call appends one span [test: crates/tracekit/tests/prose_claims.rs::claim_one_line_per_record]. The `/condukt` skill's shell steps call it [test: crates/tracekit/tests/prose_claims.rs::claim_condukt_skill_calls_record]:

```sh
tracekit record --run RID-42 --span t1 --name "interpret goal" \
  --phase interpreter --model sonnet --ms 1840 --cost 0.012 --status ok
tracekit record --run RID-42 --span t2 --parent t1 --name "impl auth" \
  --phase worker --model opus --ms 30200 --cost 0.41 --status verified
```

Spans append to `~/.tracekit/<RID>/spans.jsonl`: under concurrent records an
existing line stays first and each concurrent span id appears exactly once in
the file [test: crates/tracekit/tests/prose_claims.rs::claim_store_path_append_concurrent].

[record: 2026-10-15] 16 concurrent `tracekit record` processes for one run sometimes wrote two spans onto one line (4 of 15 runs), and the loader skips such a line as malformed.

`--task` attaches a task id (exported as
`harness.task_id`) [test: crates/tracekit/tests/prose_claims.rs::claim_task_exported]. `--ms` defaults to 0 and `--status` to `ok` [test: crates/tracekit/tests/prose_claims.rs::claim_defaults_ms_status]. `--end-unix-ms`
overrides the record-time end stamp [test: crates/tracekit/tests/prose_claims.rs::claim_end_unix_ms_override].

```sh
tracekit trace RID-42
```
```
trace RID-42
· interpret goal [interpreter/sonnet] 1840ms $0.0120 ok
  · impl auth [worker/opus] 30200ms $0.4100 verified

  2 spans · wall 31480ms · $0.4220 · slowest impl auth (30200ms) · 0 error(s)
```

Failed phases are marked `✗` and counted in the roll-up. [test: crates/tracekit/src/trace.rs::summary_counts_errors_and_marks_them]

```sh
tracekit export RID-42                 # → ~/.tracekit/RID-42/otlp-RID-42.json [test: crates/tracekit/tests/prose_claims.rs::claim_export_default_path]
tracekit export RID-42 --out -         # → stdout [test: crates/tracekit/tests/prose_claims.rs::claim_export_stdout]
tracekit export RID-42 --service condukt
```

The document is OTLP/JSON (`resourceSpans → scopeSpans → spans`) with GenAI
agent-span attributes: `gen_ai.operation.name` (`invoke_agent` for agent phases,
`execute_tool` for tool phases), `gen_ai.request.model`, `gen_ai.usage.cost_usd`,
plus `harness.phase` / `harness.task_id` / `harness.status` [test: crates/tracekit/tests/prose_claims.rs::claim_otlp_shape_attributes]. Parent links are
preserved as `parentSpanId`. [test: crates/tracekit/src/otlp.rs::parent_link_resolves_to_same_hex]

See the
[OTel GenAI agent-span semconv](https://opentelemetry.io/docs/specs/semconv/gen-ai/gen-ai-agent-spans/).

```sh
tracekit list      # runs that have recorded spans [test: crates/tracekit/tests/integration.rs::record_then_trace_round_trips] [test: crates/tracekit/src/main.rs::populated_base_dir_lists_runs_with_spans]
```

tracekit itself ships no hook [test: crates/tracekit/tests/prose_claims.rs::claim_no_hook]. The `/condukt` skill (`crates/condukt/skills/condukt/SKILL.md`)
contains a `command -v tracekit` check and `tracekit record` calls for an `interpret`
root span, a worker span with parent `interpret`, and a verifier span whose parent is the worker span [test: crates/tracekit/tests/prose_claims.rs::claim_condukt_skill_calls_record]. No file under
`crates/condukt/src` mentions `tracekit` or `spans.jsonl` [test: crates/tracekit/tests/prose_claims.rs::claim_condukt_binary_no_spans].
