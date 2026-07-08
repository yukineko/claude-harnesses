> **REVIEW-NEEDED** — この仕様は実装から逆算生成した draft。人間レビューまで正典として扱わない。

# curate 仕様

## 概要

`curate` は fugu-router の playbook（検証済みタスクの追記ログ）を、evalkit が消費する
**バージョン管理された golden eval データセット**へ昇格させる CLI である。オフライン eval
ループの供給側（supply side）に位置づく。fugu-router は検証を通った各タスクを
`~/.fugu-router/playbooks.jsonl` に append-only で記録する（policy-search 専用で holdout
整理はされない）。curate はそこから選んだ 1 件を read-only で読み、evalkit の golden ケース
（`input→expected` テスト）として `evals/curated/<name>.jsonl` に追記する。evalkit は
`evals/` を再帰探索するので、昇格ケースは設定変更なしで拾われる。

蒸留は **honest mapping** に立つ——playbook は手順（procedure）でありテストそのものではない。
受け入れ基準（`done_criteria`）が**機械的**なときだけ実行可能ケースを自動導出し、それ以外は
**draft**（人間がアサーションを書くまで evalkit がスキップ）にする。ライフサイクル hook では
なく、人間が直接叩くか condukt の Phase 6（record 後）が HOTL で提案する素の CLI。バンドル
された単一 Rust バイナリで完結（subscription-native、API キー不要）。

## 不変条件

- **playbook 非改変（read-only 消費）** — `seed::load` は playbook store を
  `read_to_string` するのみで書き戻さない。curate が書くのは `evals/curated/<name>.jsonl`
  だけ（`append_golden`）。
- **fugu struct から decouple** — `seed::Seed` は必要なフィールド（`ts`/`title`/
  `done_criteria`）のみ deserialize し、`class`/`touched_files`/notes 等の未知フィールドは
  無視する。fugu 側の struct 変更に引きずられない。
- **corrupt line 耐性（fail-soft）** — `seed::load` は空行と malformed 行を `filter_map`/
  `.ok()` で読み飛ばす。store 欠落は空 `Vec`。`existing_ids` も読めない dataset を空集合と
  みなす。1 行の破損が curation 全体を沈めない。
- **id による決定論的重複排除** — 昇格前に `existing_ids`（dataset 内の全 `id`、`//`
  コメント行と空行は無視）を集め、`derive::slug_id` が導く id が既存なら追記せずスキップする。
  同一 playbook の二重昇格を防ぐ。
- **id の安定性と衝突耐性** — `slug_id` はタイトルの ASCII slug に `DefaultHasher` の
  24bit ハッシュ（`{stem}-{:06x}`）を付す。非 ASCII/日本語タイトルは stem が空になり
  `case-` 接頭辞になるが、ハッシュで区別され安定（同一入力→同一 id）。
- **honest mapping（機械的判定のみ自動化）** — `derive::mechanical_cmd` が argv を導けた
  ときだけ `assert.exit=0` の runnable ケースにする。導けなければ、または `--draft` 指定時は
  `draft:true` にして人間へ委ねる。機械的でも `--draft` は draft を強制する（review-before-trust）。
- **filesystem-safe な dataset 名** — `sanitize` が英数・`-`・`_` 以外を `_` に写像し、
  `--dataset` からのパストラバーサル/不正文字を封じる。

## 振る舞い

サブコマンドは clap の `Command` enum（`Candidates`/`Promote`）。エラー時は `curate: {e:#}`
を stderr に出し exit 1。

- **`candidates [--store] [--k]`** — playbook store（既定 `seed::default_store()` =
  `~/.fugu-router/playbooks.jsonl`、`--store` で上書き）を読み、新しい順（`(ts, i)` 降順）に
  最大 `k`（既定 20）件を `mech `/`draft` 別に一覧する（`derive::is_mechanical` で判定、
  `done_criteria` は 60 文字に切り詰め）。store 空なら通知して exit 0。書き込みなし。
- **`promote <selector|--latest> [--dataset] [--draft] [--store] [--root] [--evals-dir]`** —
  1 件を golden へ昇格。`selector`（title 部分一致・大文字小文字無視・最新一致優先）か
  `--latest`（`seed::select`）で選び、無指定なら bail。`derive::derive_golden` で golden を
  生成し、出力先は `<root>/<evals_dir>/curated/<sanitize(dataset)>.jsonl`（既定
  `./evals/curated/promoted.jsonl`）。id が既存なら skip、無ければ `append_golden`（curated
  dir を初回に作成）で 1 行追記し、runnable/draft の別を stderr に報告する。

golden 生成（`derive::derive_golden`）:

- **runnable** — `mechanical_cmd` が argv を返し `--draft` 無しのとき
  `{id, describe:title, cmd, assert:{exit:0}}`。
- **draft** — それ以外／`--draft` 時 `{id, describe: draft_describe, draft:true}`。
  `draft_describe` は `done_criteria` を `TODO assert done_criteria: …` として埋め込み、
  空基準なら `TODO: add a file/cmd assertion`。

機械的シグナル（`mechanical_cmd`、強い順）:

1. **backticked command**（`backticked_command`） — `` ` … ` `` 内の最初のトークンが
   既知コマンド語（`is_command_word`: cargo/npm/pnpm/yarn/pytest/go/make/bash/sh/python/
   python3、または `./` 始まり）なら span をそのまま argv 化。単なる backtick 識別子は不採用。
2. **test-runner の散文言及**（`test_runner_command`） — `cargo test`（`-p <crate>` scope
   があれば捕捉）/ `npm test` / `pytest` / `go test` を正規化 argv にする。

## 構成

Rust バイナリクレート（skill/hook/agent は同梱しない。plugin.json は素の CLI プラグイン）。
3 モジュール:

- **`main`** — clap CLI（`Cli`/`Command`/`CandidatesArgs`/`PromoteArgs`）、`cmd_candidates`/
  `cmd_promote`、dataset パス解決と dedup（`existing_ids`）・追記（`append_golden`）・
  名前正規化（`sanitize`）。
- **`derive`** — seed → evalkit golden の蒸留。`derive_golden`/`is_mechanical`/
  `mechanical_cmd`/`backticked_command`/`test_runner_command`/`is_command_word`/
  `draft_describe`/`slug_id`。honest mapping の中核（純関数）。
- **`seed`** — fugu playbook store の read-only 読み取り。`Seed`（`ts`/`title`/
  `done_criteria`）、`default_store`（`harness_core::config::home` 依存）、`load`（fail-soft）、
  `select`（最新一致選択、純関数）。
