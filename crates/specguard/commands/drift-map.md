---
description: 独立した spec/feature ↔実装↔テスト↔API マッピング store (`specguard map`) を保守し、仕様書が無い entry には生成し、drift した entry は仕様と実装を突き合わせて是正する。書き込みを伴う (read-only な `/specguard:run` とは対照的)。
argument-hint: "[--baseline <ref>]"
allowed-tools: Bash, Task, Read, Write, Edit, AskUserQuestion
---

あなたは specguard の **マッピング保守エージェント** です。決定的な skeleton
(パス単位の追跡) は `specguard map` バイナリサブコマンドに委譲し、**意味的な
帰属付け** (どのファイルがどの feature/endpoint を実現するか、仕様が無い entry
への仕様生成、drift した entry の是正) はこのセッションで **あなた自身が**
行います。

このコマンドは **書き込みます** (仕様の生成、コードまたは仕様の修正)。読み取り
専用の `/specguard:run` (仕様↔実装の監査のみで、判定を subagent に委譲し修正は
一切しない) とは対照的です。混同しないでください。

追加引数: `$ARGUMENTS` (例: `--baseline HEAD~10`)。空なら付けない。

map store は **このコマンド専用ではありません**。`.specguard/spec-map.toml` は
`specmap.rs` が定義する独立レイヤーで、将来の `spec-audit` など他の機能もこの
同じ store を読み書きします。ここで store のスキーマや sync ロジックを
再実装しないこと — 常に `specguard map` バイナリに委譲してください。

以下の手順を **順番に** 実行してください。

## 1. マッピングを保守する (ハーネス: 決定的 skeleton sync)

`.specguard/spec-map.toml` の有無を `Read` (または `Bash test -f`) で確認する。

- **存在しない場合 (初回)**: `specguard map build $ARGUMENTS` を実行する
  (create-if-absent + `baseline_ref`/`fallback_ref` の全履歴窓で丸ごと seed する)。
- **存在する場合**: `specguard map sync $ARGUMENTS` を実行する (記録済み
  baseline から `git log --name-status` の差分だけを増分反映する)。

いずれも Added→新規 entry、Modified→`changed` マーク、Renamed→移動、
Deleted→detach/`missing` を **決定的に** (LLM 判定なしで) 行います。このコマンド
は skeleton を **再実装せず**、常にこのサブコマンドに委譲してください。

続けて `specguard map list --json` を実行し、現在の entries を把握する。各
entry は `key` / `kind` (Feature|Endpoint) / `spec_doc` / `status`
(Tracked|Changed|Missing) / `last_ref` / `impl_files[]` / `test_files[]` /
`client_refs[]` / `api` ({method, route}) を持つ。

## 2. 各 entry の仕様書を参照する

`spec_doc` フィールド (既定ディレクトリは `[map].spec_doc_dir`、通常
`docs/specs/`) を辿り、entry ごとの仕様書を確認する。`status: tracked` かつ
`spec_doc` がある entry は次のステップ 4 (drift 是正) の対象になり得る。
`spec_doc` が空/未設定の entry はステップ 3 の対象になる。

## 3. 仕様が無い entry には仕様を生成する (書き込み)

`spec_doc` が空、または `status: missing` の entry について:

1. その entry の `impl_files` (と `test_files` があれば) を `Read` で読む。
2. 同 crate に `specforge` (`specforge gather`/`specforge prompt` 等) が
   利用可能なら、それを使って draft 用コンテキストを集めてよい (必須ではない
   — 無ければ手順1で読んだコードから直接執筆する)。
3. 概要 / 不変条件 / 振る舞い の3節を持つ仕様書本文を執筆し、
   `docs/specs/<module>.md` (または `[map].spec_doc_dir` 配下の適切なパス) に
   `Write` する。
4. 生成した仕様書の冒頭に **`REVIEW-NEEDED`** マーカーを明記する — コードから
   逆算した仕様は「実装のバグを仕様として固定してしまう」drift 逆流リスクを
   常に伴うため、人間のレビューを経るまでは正典として扱わない。
5. entry に生成した `spec_doc` パスを記録する (`specguard map` に専用の書き込み
   コマンドが無い場合は、次回の `sync`/`list` 運用に委ねてよい。store の
   スキーマを手で書き換えて壊さないこと)。

## 4. drift を是正する (書き込み — 確信度が低ければ HOTL)

`status: changed` かつ `spec_doc` が既にある entry について、コードと仕様書の
両方を読み、突き合わせる:

- **一致している**が単に周辺ファイルが変わっただけなら、そのまま (次の
  `sync` で自然に `tracked` へ戻る運用に委ねてよい)。
- **食い違っている**場合、どちら側が誤っているかを判断し、誤っている側
  (仕様書 **または** 実装) を `Edit` で修正して整合を回復する。
- **どちらが正しい方向か判断がつかない、または確信度が低い場合は、絶対に
  黙って決め打ちしない。`AskUserQuestion` で人間に確認する (Human-on-the-loop)。**
  仕様と実装のどちらを正としてどう変更するかを問う質問を立てること。

## 多元的な帰属付け (semantic attribution)

`specguard map sync` はパスの骨格 (skeleton) しか与えない。どのファイルが
どの feature/endpoint に属するかという **意味的な** 帰属付けは、このコマンド
自身が次の情報源を **entry ごとに最も安価な情報源から順に** 読んで行う:

| 読む情報源 | 得られるもの |
|---|---|
| `git log --name-status` (`specguard map sync` 経由) | 変更された impl/test パス (安価・LLM 不要) |
| テストコード | どの feature が実際にどのテストで演習されているか |
| API/route の実装 | どのファイルがどの feature/endpoint を実装しているか |
| クライアント側の HTTP 呼び出し (API/URL) | endpoint → サーバ実装の対応 (`api` + `client_refs` を埋める) |
| API/feature を記述した仕様書 | 仕様側から entry を起こす (spec-doc 起点の帰属) |

## コスト制約付き解決 (原則)

- **永続化された map と決定的シグナルを、開放的な repo 探索より優先する。**
  store が存在する目的は「毎回すべてを再推論しない」ためであり、安価な層
  (skeleton sync + 上表の決定的シグナル) で足りる限り、それ以上のコードの
  意味的読解には踏み込まない。対象 entry にとって安価な層が不十分なときだけ、
  必要最小限をコードを読んで補う。
- **安価に解決できない帰属付け/生成に出会ったら**、次のどちらかを選ぶ:
  (a) `AskUserQuestion` で人間に聞く、または (b) 未解決のまま扱う (entry を
  `missing`/未着手のままにして次に進む)。
  **無際限な探索を続けて無理に解決しようとしてはならない** — コストは
  常に有界に保つこと。

## 注意

- `/specguard:run` は read-only な監査 (subagent に判定だけ委譲し、修正は一切
  しない) であるのに対し、このコマンドは **書き込みます** (仕様生成 + drift
  是正)。両者は役割が異なるので、監査だけしたいときは `run`、マッピングを
  育てたり是正したりしたいときは `drift-map` を使う。
- map store (`.specguard/spec-map.toml`) はこのコマンド専用ではない独立レイヤー
  であり、将来の `spec-audit` からも共有される。このコマンドは skeleton の
  sync ロジックを再実装せず、常に `specguard map build`/`sync`/`list` に委譲する。
- ステップ4の HOTL ゲートは、仕様↔実装のどちらを正とするか自動で断定できない
  ときの安全弁である。確信度が低いまま黙って一方を書き換えて「解決した」と
  報告しないこと。
