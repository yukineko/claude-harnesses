- `ToolResultGroomer` は要約文を書かない：予算超過のツール結果の先頭と末尾を逐語のまま残し、中間だけを省略マーカー `…[context-governor: elided N chars of TOTAL]…` に置き換える。
- `Pinned` / `Verbatim` のアイテムからは `Evictable` トークンを生成できず、生成できるのは `Lane::Evictable` のアイテムだけである。
- `Stop` と `SubagentStop` はそれぞれ厳密に `{}` を出力して exit 0 する。

hook-dispatch バイナリは stdin でフックペイロードを受け取り、JSON エンベロープを stdout に書く。
`hooks/hooks.json` はこの 1 つのバイナリを `PostToolUse`（マッチャ `Read|Bash|Grep|Glob|WebFetch|BashOutput|NotebookRead|Task`）・`UserPromptSubmit`・`SessionStart`・`PreCompact`・`Stop`・`SubagentStop` に登録している。
有効なペイロードに対し、`PostToolUse` / `UserPromptSubmit` / `SessionStart` / `Stop` / `SubagentStop` はいずれも stdout に JSON オブジェクトを出して exit 0 する。

- `ToolResultGroomer` は、トークン予算を超えたツール結果を、省略マーカー付きの厳密により小さい本文に置き換える。
- `ToolResultGroomer` は、トークン予算内のツール結果には手を加えない。
- `ContextInjector` は `CONTEXT_GOVERNOR_REFERENCE_DOC` が指す参照ドキュメントを読み、見出しに一致するプロンプトに対して `additionalContext` エンベロープを返す。
- `StateRehydrator` は保存済みスナップショットの pinned セクションを常駐コンテンツとして再注入する。
- 同梱の `SessionStart` マッチャは `compact` を含み、そのマッチャ経由の `compact` SessionStart はピンを再注入する。
- `Checkpointer` は、トークン使用量が閾値を超える実在のトランスクリプトを与えられたときスナップショットを記録する。

`ToolResultGroomer`・`ContextInjector`・`CompactionGuard` はそれぞれ台帳に 1 行の JSONL を追記し、各行は数値の `saved_tokens` と `resident_tokens` を持つ。

- 台帳ファイルは `<state_dir>/<project_key>/<session>/ledger.jsonl` である（`<state_dir>` は `CONTEXT_GOVERNOR_STATE_DIR`、`<session>` は `CLAUDE_CODE_SESSION_ID` から導かれる）。
- groom の行の `event` は `groomed`、inject の行は `injected`、snapshot の行は `snapshotted` である。
- `context-governor rollup` は台帳から `rows`・`total_saved_tokens`・イベント別件数を出力し、2 回実行しても出力は同一で、台帳ファイルを変更しない。
