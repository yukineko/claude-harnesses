> **REVIEW-NEEDED** — この仕様は実装から逆算生成した draft。人間レビューまで正典として扱わない。

# run-book 仕様

## 概要

`runbook` は Claude Code 向けの「再利用可能な手順インクルード」ハーネスである。ユーザーが
プロンプトに書いた `!name` マクロを、リポジトリにコミットされた手順書（`.runbook/<name>.md`）へ
展開して注入する `UserPromptSubmit` フック（`inject_hook` in `main.rs`）が正典の入口となる。
繰り返すワークフロー（デプロイ / リリース発行 / インシデント対応）を、毎回まったく同じ手順で
走らせることを目的とする。手順は 1 ファイル 1 手順の素のマークダウンで、project（`.runbook/`、
コミット対象）または global（`~/.runbook/runbooks/`）に置き、ファイル名の語幹（stem）が
マクロ名になる（`store::normalize_name`）。バンドルされた単一の Rust バイナリで動き、API キー不要。
Devin の Playbooks に着想を得ている。同 harness の `playbook`（*事実*を関連度スコアで自動注入）とは
異なり、runbook は名前で明示指名されたときだけ*手順*全体を注入する。

## 不変条件

- **注入のみ・ターン非ブロック** — フックは `run_hook(inject_hook)` 経由の best-effort で、要求された
  手順を注入するだけ。`RUNBOOK_DISABLE=1`（`Config::disabled_env`、空/`0` 以外で真）または
  `cfg.enabled=false`、stdin パース失敗（`HookInput::parse` が `None`）のいずれでも黙って return し、
  常に exit 0。ターンを止めることは決してない。
- **既存手順に解決できたときだけ発火** — `inject::expand` はスキャンした token を実在 runbook
  （`Runbook::matches`：name または alias に一致）へ解決できた場合のみ注入対象に加える。散文やコード中の
  `!`（`x != y`、`!!`、`foo!bar`）は何も注入しない。マクロ token は行頭または opener（whitespace や
  `( [ { , 、 。 「 （ 【`：`inject::is_opener`）直後の `prefix` + `[A-Za-z0-9][A-Za-z0-9_-]*` のみ拾う
  （`inject::scan_tokens`）。
- **文字数バジェット（ハード上限）** — 注入テキスト総量は `cfg.max_chars`（既定 12000、下限 200）で
  cap し、1 手順あたりは `cfg.per_runbook_chars`（既定 4000、`[200, max_chars]` に clamp）で切り詰める
  （`harness_core::inject::CharBudget` / `truncate_chars`）。予算超過分は「…（残り N 件…省略）」で打ち切る。
- **重複排除と順序** — 同一 runbook（name 一致）は初出順で 1 度だけ注入される（`expand` 内の dedup）。
  alias 経由の重複指名も同じ手順なら 1 件に畳まれる。
- **project が global を shadow** — `Store::load_all` は project → global の順に読み、同名 global は
  project に隠される。結果は name でソートし決定論的。
- **install の非破壊性** — `install`/`uninstall` は冪等で、`~/.claude/settings.json` を書く前に
  バックアップを取り、他プラグインの hook グループ（`MARKERS=["runbook"]` に該当しないもの）を保持する
  （`harness_core::install` 経由）。
- **config は非マージのレイヤ選択** — project `runbook.toml` → global `~/.runbook/config.toml` → 組込み
  default の 3 層で、最初に存在するファイルが勝ち、レイヤは merge しない（`Config::load` /
  `harness_core::inject::load_layered`）。

## 振る舞い

サブコマンドは `clap` の `Command` enum（`main.rs`）で定義。

- **`inject`（フック本体）** — `UserPromptSubmit` hook。stdin の `HookInput` を読み、`cwd_or_current` で
  root を解決 → `Config::load` → `Store::load_all` で全 runbook を読み → `inject::expand` で `!name` を
  解決 → `inject::render` が注入テキスト（`HEADER` + 各手順ブロック）を返せば stdout へ出力（=追加
  コンテキストとして注入）。注入時は `harness_core::inject_metrics::record("run-book", …)` で計測を記録する。
- **`list`** — project/global の全 runbook を `[scope] !macro — description` 形式で一覧。空なら作成方法を案内。
- **`show <name>`** — `normalize_name` で正規化した key に一致する 1 手順の scope/path/body を表示。
  無ければ stderr へ案内して exit 1。
- **`new <name> [--description] [--global] [--force]`** — `TEMPLATE`（Overview / Procedure /
  Specifications / Forbidden Actions を持つ scaffold）から `<slug>.md` を生成。既存かつ `--force` 無しは
  exit 1。`--global` で global dir へ。
- **`init`** — project `.runbook/` を作り `example.md` サンプルを（無ければ）配置し、`!example` /
  `!<index_token>` の試し方を案内。
- **`install [--dry-run]` / `uninstall [--dry-run]`** — スタンドアロン（`cargo install`）用に
  `UserPromptSubmit` フックを `~/.claude/settings.json` にマージ / 除去（`install.rs`。プラグイン導入時は
  `hooks/hooks.json` が配線するので不要）。
- **`status`** — 解決済み config ソース・`enabled`・`prefix`・`index_token`・project/global dir・
  `include_global`・`max_chars`・`per_runbook_chars`・可視 runbook 件数を表示。
- **index メタマクロ** — `!<index_token>`（既定 `!runbooks`）は手順本体ではなく利用可能な runbook 一覧を
  注入する（`render_index`）。ただし同名の実 runbook があればそちらを優先（`expand` の `index_is_real`）。

### module 責務

- **`main`** — CLI（`clap` `Cli`/`Command`）と各サブコマンドの実装、`inject_hook`、scaffold 用 `TEMPLATE`。
- **`config`** — `Config`（`enabled`/`project_dir`/`global_dir`/`include_global`/`prefix`/`index_token`/
  `max_chars`/`per_runbook_chars`）と `FileConfig`（TOML）。3 層解決（`load`）、`RUNBOOK_DISABLE`
  判定（`disabled_env`）、char 予算の clamp、path 解決（`project_runbook_dir`/`expand_or_join`）。
- **`store`** — 手順ストア。`Runbook`/`Meta`（`+++` TOML フロントマター、`parse`）、`normalize_name`
  （lowercase・`[a-z0-9_-]`・最大 48 文字）、`Store::load_all`（project→global、shadow）、
  `read_dir_runbooks`（`.md` のみ、body 空はスキップ）。
- **`inject`** — プロンプト走査と注入描画。`scan_tokens`（境界付き token 抽出）、`expand`（解決・dedup・
  index 判定 → `Expansion`）、`render`（`HEADER` + 予算内の手順ブロック）、`render_index`。
- **`install`** — スタンドアロンの settings.json フック merge/remove（`harness_core::install` へ委譲）。
- **`model`** — `HookInput` を `harness_core::hook::HookInput` から re-export するだけ（薄い alias）。
