# sitecraft

サイト／Web アプリのプロトタイプ作りを型にはめる Claude Code プラグイン（v0.1.0）。

`/sitecraft` スキルが、作りたいもの（目的）から必要な設計判断を洗い出してユーザーに**全部**決めてもらい
（`sitecraft/decisions.toml` に記録）、記録が揃ってから scaffold・実装・検査へ進む。このクレートは
決定論の側: 決定記録とその検査、dev server の管理、壊れたページとセキュリティの検査、Stop gate。

## CLI

| コマンド | 役割 |
|---|---|
| `sitecraft decide next` | 次の未回答の問いを JSON で出す（`{"status":"ask",…}` / `{"status":"done"}`） |
| `sitecraft decide purpose --text T` | 目的の要約を `[purpose] text` に記録する（前後の空白は除く）。空白だけの `--text` は記録を変えずに exit 1。`decide next` は目的を問わない |
| `sitecraft decide record --area A --question Q --answer V [--answer V2]… [--note T]` | 回答を記録する。`--question relevance` に `relevant` / `irrelevant` で領域の関連を判定。`--area domain` で追加したドメイン問いに答える。`--answer` を繰り返せるのは multi の問い（カタログでは `notification.channel` だけ）で、1 個でも配列で保存する（multi でない問いに 2 個以上は失敗終了）。`--note` は補足（カタログ回答は `[areas.<a>.notes]`、ドメイン問いは `note`。付けない再記録は既存の補足を残す） |
| `sitecraft decide check` | 目的と必要な判断がすべて記録済みで、整合性規則に違反しなければ 0。未回答・目的未記録（`sitecraft decide purpose --text` の案内付き）・規則違反があれば 1。記録が無い・壊れている（multi でない問いに配列がある等）なら 2 |
| `sitecraft decide add-question --id I --text T [--choice C]… [--multi]` | 目的から起こしたドメイン固有の問いを追加する（`--choice` 無しは自由記述、`--multi` で複数選択） |
| `sitecraft decide classify --feature F --kind simple\|dedicated-harness [--harness P] [--confirm-fit yes\|no]` | 機能を分類し、`dedicated-harness` なら `$SITECRAFT_MARKETPLACE` の marketplace.json に既存 plugin があるか（`existing`）・その `description`・`reuse`（`unconfirmed` / `confirmed` / `rejected` / `not-applicable`）を報告する。`--confirm-fit` はユーザーの適合判断を `[features.F] fit` に記録し、`existing: true` のときだけ受け付ける |
| `sitecraft serve start` / `stop` / `status` | trust 済み `sitecraft.toml` の `command` を自前のプロセスグループで起動・停止・状態表示する（ポート空き確認、ready 待ち、停止はポート解放まで確認） |
| `sitecraft check pages [--base-url URL]` | 同梱の `assets/check-pages.mjs` を Playwright で回し、console error・page error・asset の HTTP エラー・壊れたリンクを検出する |
| `sitecraft check security` | build 出力の秘密情報（伏字表示）と `.env` 値の混入、ソースの危険な HTML sink、`npm audit --json` の high/critical を検出する |
| `sitecraft trust` | 現在の `sitecraft.toml` の sha256 を trust store に記録する（人間が実行する） |
| `sitecraft gate` | Stop hook（stdin の Stop payload を読む） |

全サブコマンド共通で `--root <dir>` を取る（既定は cwd。`gate` は payload の `cwd` を使う）。

### 整合性規則（`decide check`）

規則は `src/decide/rules.rs` の閉じた表で、次の 6 行がすべて（追加・削除は
`docs/specs/sitecraft-capabilities-rulings.md` の D7 の改訂を要する）。対象はカタログ回答だけで、ドメイン問いは判定しない。

| id | 条件 | 判定 |
|---|---|---|
| W1 | auth.relevance=relevant かつ exposure.relevance=irrelevant | warning（stderr に `warning: W1: …`。終了コードは変えない） |
| V1 | exposure.hosting=static かつ processing.execution ∈ {in-request, background} | violation |
| V2 | exposure.hosting=static かつ storage.file-storage=local-disk | violation |
| V3 | users-roles.user-model ∈ {same-rights, multi-role} かつ（auth.relevance=irrelevant または auth.needs-login=no） | violation |
| V4 | admin.admin-ui=yes かつ（auth.relevance=irrelevant または auth.needs-login=no） | violation |
| V5 | workflow.on-failure ∈ {notify, both} かつ notification.relevance=irrelevant | violation |

記録の無い回答（領域が irrelevant・follow-up が非活性・未回答）を参照する項は偽。multi の回答はいずれかの要素が
集合に含まれれば真。violation は `<id>: <表示文> (<area>/<question> = <answer>, …)` を出して exit 1。

### 変更履歴（`[[history]]`）

既に記録された値（回答・relevance・補足・目的）が変わる記録ごとに、`[[history]]` に
`{area, question, old, new, at}`（`at` は UTC RFC 3339、補足の変更は `question = "<q>#note"`、目的は
`area = "purpose"`, `question = "text"`）を追記し、stdout に `changed: <old> -> <new>` を 1 行出す。
同じ値の再記録・初めての記録は history を増やさない。follow-up の剪定や relevance の変更で消える回答は
`new = ""`, `pruned = true` の行として残る。

## 終了コード規約

`0` clean / `1` violation / `2` undetermined。各コードの意味の正典は
[`skills/sitecraft/SKILL.md`](skills/sitecraft/SKILL.md) の「終了コード規約」節。

**`2` は合格ではない。** 呼び出し側は block として扱う。`gate` だけは Stop-hook プロトコル
（exit 0 + stdout の `{"decision":"block","reason":…}`）で話す。

## Stop gate

`sitecraft gate` は `harness_core::gate::run::run_guarded`（gate 名 `sitecraft`）の panic barrier 下で走る
（panic は block。同一セッションで連続 2 回目の panic だけが bounded に通る）。

- `sitecraft.toml` が無い repo — 通す（プロジェクトが opt-in していない）。state dir の `gate-log.jsonl` に
  `not-configured` を 1 行追記する。
- 設定が読めない・未 trust・trust 後に変更 — block（`sitecraft trust` の案内付き）。`command` は起動しない。
- 以下を `Verdict::worst_of` で合成する:
  - `decide check` の判断部分（未回答・整合性規則 V1〜V5 の違反は block、`sitecraft decide next` の案内付き）。
    **gate は `[purpose]` を要求しない**: 目的の未記録で exit 1 にするのは CLI の `sitecraft decide check` だけ
  - `check security`
  - `serve start` → `check pages` → `serve stop` — セッションの transcript に `web_globs` に一致するファイルへの
    Write/Edit/MultiEdit/NotebookEdit があるときだけ。transcript が無い・読めない・空・パースできない行がある・
    書き込み先の無い書き込み呼び出しがある・sub-agent が走った（その編集は別の transcript にある）ときは
    変更有無を判定不能として**実行する**（省略側に倒さない）。gate が起動した dev server は drop guard で必ず止める。
- stdin が空でない非 JSON — block。空の stdin は手動実行（判定を stderr、終了コードは上の規約）。
- give-up は bounded: 同一セッションで `max_attempts`（trust 済み設定の値を 1..=10 に clamp。未 trust なら既定 3）
  回 block した次の stop は、`NOTHING WAS VERIFIED` を含む `systemMessage` 付きで通す。

`bin/sitecraft` launcher はプラットフォーム別バイナリが無いとき、`gate` には block JSON を返し
（同一セッションでこの gate の連続 2 回目の失敗だけ通す）、それ以外のサブコマンドは stdout 空で非 0 終了する。

## sitecraft.toml

プロジェクト直下に置く（キーはすべてトップレベル。未知のキーはエラー）。全項目つきの例と各項目の説明は
[`sitecraft.example.toml`](sitecraft.example.toml)（これをコピーして編集する）。

## trust の手順

`command` は `serve start` と Stop gate が実行するので、承認していない内容は走らせない（webqa と同じ方式）。

1. `sitecraft.toml` を書く（または編集する）。
2. **人間が**内容を確認し、プロジェクト直下で `sitecraft trust` を実行する。canonical な project root を
   キーに、ファイル内容の sha256 が `$SITECRAFT_STATE_DIR/trust.json`（既定 `~/.sitecraft/trust.json`）に記録される。
3. 以後、ファイルの内容が変わると未 trust に戻り、`serve start` は exit 2、gate は block する。もう一度 1.–2. を行う。

LLM（`/sitecraft` スキル）は `sitecraft trust` を代わりに実行しない。

## webqa との分担

- **webqa** — 変更した Web コードに対応する Playwright spec を走らせて Stop を止める E2E ゲート。spec の生成は
  `/webqa-gen` スキル（RED 観測・弱体化検査込み）。
- **sitecraft** — spec を持たない検査を受け持つ: 設計判断が全部記録されたか、設定したページが開いて
  エラーを出さないか（spec 不要の smoke）、秘密情報・危険な sink・脆弱な依存が無いか。
- `/sitecraft` スキルは E2E を `/webqa-gen` に委ね、Playwright のテスト生成を再実装しない。両方を入れた
  プロジェクトでは Stop で両方の gate が走る。

## 専用 harness への切り出し方

独立した検査・ゲート・状態管理が要る機能は、サイトの中に埋め込まず専用 plugin に切り出す。

1. `sitecraft decide classify --feature <name> --kind dedicated-harness [--harness <plugin>]` を
   `SITECRAFT_MARKETPLACE=<harness repo>/.claude-plugin/marketplace.json` 付きで実行する。
2. 出力 JSON の `existing` が `true` なら、`description` をユーザーに示して目的に合うかを聞き、その回答だけを
   `--confirm-fit yes|no` で記録する。`reuse: confirmed` のときだけ既存 plugin を使う（`unconfirmed` は
   再利用の許可ではない）。`reuse: rejected` または `existing: false` なら harness repo に新規 plugin（rejected のときは別名）
   （`crates/<plugin>/` に `Cargo.toml`・`.claude-plugin/plugin.json`・`hooks/hooks.json`・`bin/<plugin>`
   launcher・`skills/`・`README.ja.md`）を作り、`.claude-plugin/marketplace.json` に登録して 3 ファイルの version を
   lockstep させる。判定は `harness_core::verdict::Verdict`（三値）で返し、判定不能は block に倒す。
3. exit 2（`SITECRAFT_MARKETPLACE` 未設定・読めない）は「既存が無い」ではない。パスを直して再実行する。

## 環境変数（テスト用の差し替え口）

| 変数 | 用途 | 既定 |
|---|---|---|
| `SITECRAFT_NODE` | `check pages` が起動する node | `PATH` 上の `node` |
| `SITECRAFT_NPM` | `check security` が `npm audit` に使う npm | `PATH` 上の `npm` |
| `SITECRAFT_STATE_DIR` | trust store・dev server 状態・gate の記録 | `$HOME/.sitecraft` |
| `SITECRAFT_MARKETPLACE` | `decide classify` が見る marketplace.json | なし（未設定は undetermined） |
