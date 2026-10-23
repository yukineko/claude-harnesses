- `ToolResultGroomer` does not write summary text: it keeps a verbatim head and a verbatim tail of an over-budget tool result and replaces only the middle with an elision marker `…[context-governor: elided N chars of TOTAL]…`.
- An `Evictable` token cannot be minted from a `Pinned` or `Verbatim` item; only a `Lane::Evictable` item yields one.
- `Stop` and `SubagentStop` each print exactly `{}` and exit 0.

The hook-dispatch binary reads the hook payload on stdin and writes a JSON envelope to stdout.
`hooks/hooks.json` registers that one binary for `PostToolUse` (matcher `Read|Bash|Grep|Glob|WebFetch|BashOutput|NotebookRead|Task`), `UserPromptSubmit`, `SessionStart`, `PreCompact`, `Stop` and `SubagentStop`.
With a valid payload, `PostToolUse`, `UserPromptSubmit`, `SessionStart`, `Stop` and `SubagentStop` each exit 0 with a JSON object on stdout.

- `ToolResultGroomer` replaces a tool result that is over its token budget with a strictly smaller body carrying an elision marker.
- `ToolResultGroomer` leaves a tool result that is under its token budget untouched.
- `ContextInjector` reads the reference document named by `CONTEXT_GOVERNOR_REFERENCE_DOC` and returns an `additionalContext` envelope for a prompt that matches one of its headings.
- `StateRehydrator` re-injects the pinned sections of the stored snapshot as resident content.
- The shipped `SessionStart` matcher covers `compact`, and a `compact` SessionStart through that matcher rehydrates the pin.
- `Checkpointer` records a snapshot when given a real transcript whose token usage is above its threshold.

`ToolResultGroomer`, `ContextInjector` and `CompactionGuard` each append one JSONL row to the ledger, and every row carries numeric `saved_tokens` and `resident_tokens`.

- The ledger file is `<state_dir>/<project_key>/<session>/ledger.jsonl`, where `<state_dir>` is `CONTEXT_GOVERNOR_STATE_DIR` and `<session>` is derived from `CLAUDE_CODE_SESSION_ID`.
- A groom row has `event` `groomed`, an inject row has `event` `injected`, and a snapshot row has `event` `snapshotted`.
- `context-governor rollup` prints `rows`, `total_saved_tokens` and per-event counts from the ledger; two runs print identical output and leave the ledger file unchanged.
