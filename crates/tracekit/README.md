# tracekit

Span-tree tracer for **condukt runs** — the missing causal view between gauge's
cost buckets and a real OTel trace.

gauge answers *"how much did agents cost?"* by **kind** (main vs sub-agent); it
has no run/task/span linkage. So when a condukt run is slow or fails, nothing
tells you *which phase* — the interpreter? a worker? the verifier? — was the
culprit. tracekit records each phase of one run as a **parent-linked span tree**
(`phase, model, ms, cost, status`), renders it, and exports it as OpenTelemetry
GenAI-semconv JSON.

Subscription-native: file-only, **no network, no API key**. (Live OTLP/HTTP push
to a backend is a planned follow-up — the exported file already matches the OTLP
`TracesData` shape, so it can be replayed later.)

## Record a span

A caller (condukt's state-set transition, or a human) appends one span per phase
as it finishes:

```sh
tracekit record --run RID-42 --span t1 --name "interpret goal" \
  --phase interpreter --model sonnet --ms 1840 --cost 0.012 --status ok
tracekit record --run RID-42 --span t2 --parent t1 --name "impl auth" \
  --phase worker --model opus --ms 30200 --cost 0.41 --status verified
```

Spans append to `~/.tracekit/<RID>/spans.jsonl` (append-only — concurrent worker
completions never clobber each other). `--task` attaches a task id (exported as
`harness.task_id`), `--ms` defaults to 0 and `--status` to `ok`. `--end-unix-ms`
overrides the record-time end stamp for deterministic replay.

## Render the tree

```sh
tracekit trace RID-42
```
```
trace RID-42
· interpret goal [interpreter/sonnet] 1840ms $0.0120 ok
  · impl auth [worker/opus] 30200ms $0.4100 verified

  2 spans · wall 31480ms · $0.4220 · slowest impl auth (30200ms) · 0 error(s)
```

Failed phases are marked `✗` and counted in the roll-up, so the slow/expensive/
broken phase of a run is obvious at a glance.

## Export OTel GenAI spans

```sh
tracekit export RID-42                 # → ~/.tracekit/RID-42/otlp-RID-42.json
tracekit export RID-42 --out -         # → stdout
tracekit export RID-42 --service condukt
```

The document is OTLP/JSON (`resourceSpans → scopeSpans → spans`) with GenAI
agent-span attributes: `gen_ai.operation.name` (`invoke_agent` for agent phases,
`execute_tool` for tool phases), `gen_ai.request.model`, `gen_ai.usage.cost_usd`,
plus `harness.phase` / `harness.task_id` / `harness.status`. Parent links are
preserved as `parentSpanId`. See the
[OTel GenAI agent-span semconv](https://opentelemetry.io/docs/specs/semconv/gen-ai/gen-ai-agent-spans/).

## Other

```sh
tracekit list      # runs that have recorded spans
```

## Integration

tracekit itself ships no hook. The `/condukt` skill (`crates/condukt/skills/condukt/SKILL.md`)
calls `tracekit record` from its shell steps as a soft dependency — an `interpret`
root span, then a worker span and a verifier span per task. The condukt binary does
not emit spans itself, and joining gauge's per-agent cost onto the matching span
is not automated.
