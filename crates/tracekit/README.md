tracekit records each phase of one run as a **parent-linked span tree**
(`phase, model, ms, cost, status`), renders it as an indented tree, and exports it as
OTLP/JSON.

## Record a span

Each `tracekit record` call appends one span. The `/condukt` skill's shell steps call it:

```sh
tracekit record --run RID-42 --span t1 --name "interpret goal" \
  --phase interpreter --model sonnet --ms 1840 --cost 0.012 --status ok
tracekit record --run RID-42 --span t2 --parent t1 --name "impl auth" \
  --phase worker --model opus --ms 30200 --cost 0.41 --status verified
```

Spans append to `~/.tracekit/<RID>/spans.jsonl`: under concurrent records an
existing line stays first and each concurrent span id appears exactly once in
the file.

 16 concurrent `tracekit record` processes for one run sometimes wrote two spans onto one line (4 of 15 runs), and the loader skips such a line as malformed.

`--task` attaches a task id (exported as
`harness.task_id`). `--ms` defaults to 0 and `--status` to `ok`. `--end-unix-ms`
overrides the record-time end stamp.

```sh
tracekit trace RID-42
```
```
trace RID-42
· interpret goal [interpreter/sonnet] 1840ms $0.0120 ok
  · impl auth [worker/opus] 30200ms $0.4100 verified

  2 spans · wall 31480ms · $0.4220 · slowest impl auth (30200ms) · 0 error(s)
```

Failed phases are marked `✗` and counted in the roll-up.

```sh
tracekit export RID-42                 # → ~/.tracekit/RID-42/otlp-RID-42.json
tracekit export RID-42 --out -         # → stdout
tracekit export RID-42 --service condukt
```

The document is OTLP/JSON (`resourceSpans → scopeSpans → spans`) with GenAI
agent-span attributes: `gen_ai.operation.name` (`invoke_agent` for agent phases,
`execute_tool` for tool phases), `gen_ai.request.model`, `gen_ai.usage.cost_usd`,
plus `harness.phase` / `harness.task_id` / `harness.status`. Parent links are
preserved as `parentSpanId`.

See the
[OTel GenAI agent-span semconv](https://opentelemetry.io/docs/specs/semconv/gen-ai/gen-ai-agent-spans/).

```sh
tracekit list      # runs that have recorded spans
```

tracekit itself ships no hook. The `/condukt` skill (`crates/condukt/skills/condukt/SKILL.md`)
contains a `command -v tracekit` check and `tracekit record` calls for an `interpret`
root span, a worker span with parent `interpret`, and a verifier span whose parent is the worker span. No file under
`crates/condukt/src` mentions `tracekit` or `spans.jsonl`.
