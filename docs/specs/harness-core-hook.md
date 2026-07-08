> **REVIEW-NEEDED** — この仕様は実装から逆算生成した draft。人間レビューまで正典として扱わない。

# harness-core::hook 仕様

## 概要

`hook` module は全プラグインの Claude Code hook バイナリが共有する I/O ユーティリティ層である。stdin から届く hook payload の型 (`HookInput` / `ContextWindow`) と、その安全な読み取り (`read_stdin` / `read_stdin_if_piped`)、実行環境の判定 (`is_headless`)、そして「hook はユーザのターンを絶対に壊さない」不変条件を強制する panic-catch 実行ラッパ (`catch_silent` / `catch_and_log` / `run_hook`) を提供する。各 hook はこの module 経由で payload を解釈し、失敗しても静かに exit 0 する。

## 不変条件

- **ターンを壊さない**: `run_hook` は handler の panic を `catch_and_log` で捕捉し、必ず `std::process::exit(0)` で終える。エラー・panic 時も 0 で抜けてユーザのターンを継続させる。
- **不正入力は静かに拒否**: `HookInput::parse` は空文字・不正 JSON を `None` で返す。欠損フィールドは serde の `#[serde(default)]` で既定値になり parse 失敗にならない。
- **stdin DoS 境界**: `read_stdin` は `MAX_STDIN_BYTES` (10 MiB) で読み取りを cap する。加えて `HookInput::parse` は `serde_json` の recursion limit (既定 128) を保つため、過剰にネストした payload を stack overflow でなく `None` で拒否する (size 系・depth 系の両 DoS を境界化)。`.disable_recursion_limit()` への差し替えはこのガードを失わせるため禁止。
- **headless での沈黙**: `is_headless` が true (stdout が TTY でない) のとき、user-facing な stdout 出力は抑制すべき。piped/captured stdout への出力は呼び出し側が parse する機械出力を汚染するため。
- **対話端末でブロックしない**: `read_stdin_if_piped` は stdin が対話端末なら `read_stdin` を呼ばず即座に空文字を返し、EOF 待ちハングを避ける。
- **純粋性**: `is_headless` は stdout fd のみ、`read_stdin_if_piped` は stdin fd のみを検査する副作用限定関数。

## 振る舞い

- **`HookInput::parse(raw: &str) -> Option<Self>`**: trim 後に空なら `None`。それ以外は `serde_json::from_str` で deserialize し、失敗時 `None`。全 hook event の payload を単一構造体で吸収する (欠損フィールドは既定値)。
- **`HookInput::cwd_or_current(&self) -> PathBuf`**: `cwd` が空なら process の current dir (取得失敗時 `"."`) にフォールバック。
- **`HookInput::project_name(&self) -> String`**: `cwd_or_current` の basename。取得不能時 `"project"`。
- **`HookInput::session_key(&self) -> String`**: `session_id` が空なら共有バケット `"_local"`、それ以外は session_id をそのまま返す。per-session state の安定キー。
- **`HookInput::target(&self) -> Option<String>`**: `tool_name` が `Edit`/`Write`/`MultiEdit`/`Read`/`NotebookEdit` のとき `tool_input` の `file_path` (無ければ `notebook_path`) を返す。それ以外は `None`。
- **`ContextWindow::total_tokens(&self) -> Option<u64>`**: `total_input_tokens` と `total_output_tokens` の和。input のみあれば input 単体、両方無ければ `None`。
- **`read_stdin() -> String`**: stdin を `MAX_STDIN_BYTES` まで読み、read エラーは握り潰し、UTF-8 を lossy decode して返す (cap での多バイト境界割れでもエラーにしない)。
- **`read_stdin_if_piped() -> String`**: stdin が端末なら空文字、piped なら `read_stdin` へ委譲。
- **`is_headless() -> bool`**: stdout が端末でなければ true。
- **`catch_silent<F>(f: F) -> bool`**: `f` を実行し panic を捕捉。完走で `true`、unwind で `false` (`run_hook` の testable core)。
- **`catch_and_log<F>(hook_name: &str, f: F) -> bool`**: `catch_silent` 同様だが、panic payload (`&str`/`String`/その他) を stderr に `[harness hook panic] <hook_name>: <msg>` 形式で出力。ターンは壊さない。
- **`run_hook<F>(f: F) -> !`**: `catch_and_log("hook", f)` を呼び、常に exit 0 する hook のトップレベル実行ラッパ。
