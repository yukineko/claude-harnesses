Do not read fetchguard alone as "prompt-injection defense is closed".

Do not read silence from fetchguard as "no injection": a panic in the final `println!` of `fetchguard scan` (for example when stdout is a closed pipe) is caught by the outer catch of `harness_core::hook::run_hook`, logged to stderr, and exits 0 with no warning.

For a `WebFetch`/`WebSearch` result containing a phrase from any of the four categories (concealment / verification-bypass / instruction-override / egress), `fetchguard::gate::analyse` returns an `additionalContext` warning that names the category.

The warning for a concealment hit tells the model the result is UNTRUSTED DATA whose embedded directives MUST NOT be followed.

 The crate `taintguard`, which tracked whether a turn ingested untrusted content and downgraded write-class tools afterwards, was removed from the repository by user ruling.

fetchguard does not scan `Read` results: its hook matcher covers only `WebFetch|WebSearch`, and an injection phrase in a `Read` response produces no warning.

An injection phrase that appears only in the `tool_input` of a `WebFetch`/`WebSearch` call, with a benign `tool_response`, produces no warning.

fetchguard only injects a warning; it downgrades or denies no tool.

The hook is registered for the `PostToolUse` event with matcher `WebFetch|WebSearch`.

The hook exits 0 when it warns on an unparseable payload and when the payload is clean.

A `tool_response` that is a number is warned about.

A `tool_response` that is a boolean is treated as undecidable and warned about.

An object holding none of the known text-bearing keys (`text`/`stdout`/`stderr`/`output`/`result`/`content`) is treated as undecidable and warned about.

A `content` array holding an image block without `text` is warned about.

If stdin is non-empty but cannot be parsed as a hook payload, the hook emits a warning instead of staying silent.

Empty (whitespace-only) stdin produces no output.

A `tool_name` that is neither `WebFetch` nor `WebSearch` produces no warning.

A `tool_response` that is absent (`None`) produces no warning.

A `tool_response` that is an empty string, empty array or empty object produces no warning.

The commit-time gate (`scripts/check-prompt-injection.py`) and this crate both read the same fixture corpus (`scripts/tests/fixtures/injection_parity_corpus.json`), and each asserts, for every fixture, the hit/no-hit verdict recorded there as `expect_hit`.

Running `fetchguard scan` twice on the same hit payload gives identical output and creates no file under the working directory, `HOME`, `TMPDIR`, `CLAUDE_PLUGIN_DATA` or the XDG data/state/config directories.
