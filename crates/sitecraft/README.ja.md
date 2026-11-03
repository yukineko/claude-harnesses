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
| `sitecraft decide check` | 目的と必要な判断がすべて記録済みで、整合性規則に違反しなければ 0。未回答・目的未記録（`sitecraft decide purpose --text` の案内付き）・規則違反、relevance 未判定の能力・relevant なのに how / done_when / verify が欠けた能力があれば 1。記録が無い・壊れている（multi でない問いに配列がある・`[capabilities]` の未知のキー・型違い・未知の verify kind 等）なら 2 |
| `sitecraft decide add-question --id I --text T [--choice C]… [--multi]` | 目的から起こしたドメイン固有の問いを追加する（`--choice` 無しは自由記述、`--multi` で複数選択） |
| `sitecraft decide classify --feature F --kind simple\|dedicated-harness [--harness P] [--confirm-fit yes\|no]` | 機能を分類し、`dedicated-harness` なら `$SITECRAFT_MARKETPLACE` の marketplace.json に既存 plugin があるか（`existing`）・その `description`・`reuse`（`unconfirmed` / `confirmed` / `rejected` / `not-applicable`）を報告する。`--confirm-fit` はユーザーの適合判断を `[features.F] fit` に記録し、`existing: true` のときだけ受け付ける |
| `sitecraft init <dir>` | 新しい site repo を作る。`<dir>` が存在しないか空なら `git init` と `<dir>/sitecraft/` を作って 0。空でない・ファイル・別の git work tree の中なら何も変えずに 1。git が無い・`git init` 失敗・git が合計 30 秒超過・IO 失敗は 2（途中で作ったものは消す）。`--root` は使わない。scaffold 先は `<dir>/app/` |
| `sitecraft serve start` / `stop` / `status` | trust 済み `sitecraft.toml` の `command` を自前のプロセスグループで起動・停止・状態表示する（ポート空き確認、ready 待ち、停止はポート解放まで確認）。ready 待ちは `127.0.0.1` と `[::1]` の両方に問い合わせ、応答した方を `url` に記録する |
| `sitecraft check pages [--base-url URL]` | 同梱の `assets/check-pages.mjs` を Playwright で回し、console error・page error・asset の HTTP エラー・壊れたリンクを検出する。node は `command` が `npm --prefix <dir>` なら `<root>/<dir>` で（そこと祖先の `node_modules` から Playwright を解決）、それ以外は root で走る。続いて `api_paths` の各パスに GET し（リダイレクトは追わない）、2xx 以外・接続失敗は 1、1 パス 10 秒超過は 2 |
| `sitecraft check security` | build 出力の秘密情報（伏字表示）と `.env` 値の混入、ソースの危険な HTML sink、`npm audit --json` の high/critical を検出する。audit は `check pages` と同じ package ディレクトリ（`command` が `npm --prefix <dir>` なら `<root>/<dir>`、それ以外は root）で走る。そこに lockfile が無ければ 2（root の lockfile で代用しない）。root 内のディレクトリに解決できない `--prefix`（symlink は解決して判定）は npm を起動せずに 2。Stop gate も同じディレクトリを監査する |
| `sitecraft decide capability --name N --relevance relevant\|irrelevant [--how H] [--done-when D] [--verify-kind K …]` | Web アプリの外の能力 `[capabilities.N]` を記録・置換する（`N` は `^[a-z0-9][a-z0-9-]*$`）。relevant なら `--how`・`--done-when`・`--verify-kind` が要る。verify kind は `builtin:blender-headless`（`--script --glb --render`）・`builtin:threejs-render`（`--page /… [--selector S]`、既定 `canvas`）・`command`（`--argv` の繰り返しと 1 個以上の `--input`）の 3 種類だけ。`--input` は glob（root 相対）。不正な入力は何も変えずに 2。変更は `[[history]]`（`area = "capability"`） |
| `sitecraft capability probe N` | 能力がこのマシンで動くかを確かめ、stdout に `{"capability","result","reason"}` を 1 行出す（`feasible` 0 / `infeasible` 1 / `undetermined` 2、120 秒で `timeout` の 2）。何も書かない。完成の根拠にはならない |
| `sitecraft check capability N` | relevant な能力の verify を実行して判定する（300 秒で `timeout` の 2）。結果を result record に書く（書けなければ 2） |
| `sitecraft trust` | 現在の `sitecraft.toml` の sha256 と、`sitecraft/decisions.toml` の `command` 能力の argv の digest を trust store に記録する（人間が実行する） |
| `sitecraft status` | check 履歴から検査ごとの状態を stdout に `{"checks":[{name, last_exit, consecutive_same_failure, elapsed_ms, state}]}` で出す（下の「check 履歴と status」）。全検査が ok / slow なら 0、failing / stuck / waiting-human があれば 1、履歴が無い・読めない・パースできない行がある・state dir や trust が判定できないなら 2（stdout は空） |
| `sitecraft gate` | Stop hook（stdin の Stop payload を読む） |

全サブコマンド共通で `--root <dir>` を取る（既定は cwd。`gate` は payload の `cwd` を使う。`init` は引数の `<dir>` を使う）。

### check 履歴と status

`decide check`・`check pages`・`check security`・`check capability <name>` は、CLI でも Stop gate 経由でも、1 回ごとに
`<state>/check-history/<sha256(正規化した root)>.jsonl` に `{"name","exit","fingerprint","elapsed_ms","timed_out","at"}` を
1 行追記する（`name` は `decide` / `pages` / `security` / `capability:<name>`）。追記できなければ（state dir が決まらない場合を含む）
その検査は exit 2。`fingerprint` は exit 0 なら null、時間切れなら `sha256("timeout")`、それ以外は stdout と stderr を連結し、
時刻（RFC 3339 / ISO 8601）→ `<time>`、絶対パス → `<path>`、数字列 → `0` の順に置換した文字列の sha256 hex。gate は各検査の
CLI と同じ報告行を（表示せずに）指紋に使う。gate の `pages` 行の `elapsed_ms` は dev server の起動・停止を含む。

`sitecraft status` の `consecutive_same_failure` は履歴の末尾から exit ≠ 0 かつ最新と同じ指紋の行が続く数。`state` は
`waiting-human`（`pages` と `builtin:threejs-render` 種の能力は `sitecraft.toml` が、`command` 種の能力は argv の digest が
いま未 trust）> `stuck`（3 以上）> `failing`（最新の exit ≠ 0）> `slow`（最新が exit 0 で `elapsed_ms` が時間制限の 50% 以上。
制限は pages が `check pages` の deadline、security が npm audit の 300 秒、能力が 300 秒。`decide` は slow にならない）> `ok`
の優先順で 1 つ。decisions.toml に無い能力の行は trust 不要として扱う（`check capability` は何も実行せずに拒否するため）。

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
  - relevant な能力ごとの result record（下の「能力（capability）」）。record が無い・exit が 0 でない・別 version の
    sitecraft が書いた・入力（`input_hash`）が変わった — block。record が読めない・壊れている・入力の hash を計算し直せない
    — block（undetermined）。reason には能力名と `sitecraft check capability <name>` が入る。**gate は Blender・Chromium・
    能力のコマンドを起動しない**（record を読んで比べるだけ）。relevant な能力が無ければ何も足さない
  - `serve start` → `check pages` → `serve stop` — セッションの transcript に `web_globs` に一致するファイルへの
    Write/Edit/MultiEdit/NotebookEdit があるとき、または `web_globs` に一致するパスを含む Bash コマンドがあるときだけ。
    transcript が無い・読めない・空・パースできない行がある・書き込み先の無い書き込み呼び出しがある・
    読み取り専用と証明できない Bash コマンド（既知の読み取り専用コマンド以外、`/dev/null` 以外へのリダイレクト、
    コマンド置換・heredoc・subshell 等）がある・sub-agent が走った（その編集は別の transcript にある）ときは
    変更有無を判定不能として**実行する**（省略側に倒さない）。gate が起動した dev server は drop guard で必ず止める。
- `decide check`・`check security`・（実行したときの）pages の段は、それぞれ check 履歴に 1 行追記する。追記できなければ
  その検査は undetermined（block）。
- stdin が空でない非 JSON — block。空の stdin は手動実行（判定を stderr、終了コードは上の規約）。
- give-up は bounded: 同一セッションで `max_attempts`（trust 済み設定の値を 1..=10 に clamp。未 trust なら既定 3）
  回 block した次の stop は、`NOTHING WAS VERIFIED` を含む `systemMessage` 付きで通す。

`bin/sitecraft` launcher はプラットフォーム別バイナリが無いとき、`gate` には block JSON を返し
（同一セッションでこの gate の連続 2 回目の失敗だけ通す）、それ以外のサブコマンドは stdout 空で非 0 終了する。

## sitecraft.toml

プロジェクト直下に置く（キーはすべてトップレベル。未知のキーはエラー）。アプリは `app/` に置くので、例は
`command = ["npm", "--prefix", "app", …]`・`build_dir = "app/dist"`・`web_globs = ["app/src/**", …]` の形。
任意キー `api_paths`（`/` で始まる文字列の配列。既定は空。`/` で始まらない要素は設定エラー＝exit 2）は `check pages` が
GET で検査する。trust はファイル全体が対象なので、`api_paths` を変えても再 trust が要る。全項目つきの例と各項目の説明は
[`sitecraft.example.toml`](sitecraft.example.toml)（これをコピーして編集する）。

## trust の手順

`command` は `serve start` と Stop gate が実行するので、承認していない内容は走らせない（webqa と同じ方式）。

1. `sitecraft.toml` を書く（または編集する）。
2. **人間が**内容を確認し、プロジェクト直下で `sitecraft trust` を実行する。canonical な project root を
   キーに、ファイル内容の sha256 が `$SITECRAFT_STATE_DIR/trust.json`（既定 `~/.sitecraft/trust.json`）に記録される。
3. 以後、ファイルの内容が変わると未 trust に戻り、`serve start` は exit 2、gate は block する。もう一度 1.–2. を行う。

LLM（`/sitecraft` スキル）は `sitecraft trust` を代わりに実行しない。

`command` 能力の argv も同じ扱い: `sitecraft trust` は `sitecraft/decisions.toml` の `command` 能力を名前順に並べた
`[[name, argv], …]` の JSON の sha256 を、キー `<canonical root>#capabilities` に同じ書き込みで記録する（`command` 能力が
無ければそのキーを消す）。argv が変わると `check capability` は何も実行せずに exit 2（`sitecraft trust` の案内付き）。
trust store が読めないときも何も実行せずに 2。`decisions.toml` が読めない・`[capabilities]` が壊れているときは
`sitecraft trust` 自体が何も書かずに 2。how / done_when / builtin の verify を変えても trust は外れない。

## 能力（capability）

Web アプリの外の成果物（Blender での書き出し・three.js の描画・変換 CLI 等）は `[capabilities.<name>]` に記録し、
完成を機械判定する。

- **probe**（`sitecraft capability probe`）— 事前確認。`builtin:blender-headless` は Blender の探索
  （`SITECRAFT_BLENDER` が空でなければそれだけ。無い・実行不能なら undetermined。未設定なら `PATH` の `blender`、
  次に `/Applications/Blender.app/Contents/MacOS/Blender`、どれも無ければ infeasible）のあと、既定シーンを GLB に書き出す
  最小 script を headless で走らせて GLB ヘッダを確かめる（一時ファイルは state dir の `probe-tmp/` に作って消す）。
  `builtin:threejs-render` は同梱の `assets/capability-threejs.mjs` で headless Chromium を起動し、webgl2 / webgl の context が
  取れるかを見る。`command` は `argv[0]` が実行可能かだけを見る（trust を要求せず、実行もしない）。
- **check**（`sitecraft check capability`）—
  - `builtin:blender-headless`: 古い GLB / render を消してから
    `<blender> --background --factory-startup --python <script> -- --glb <絶対パス> --render <絶対パス>` を root で走らせる。
    GLB は先頭 4 バイト `glTF`・version 2・ヘッダの長さ＝ファイル長、render は描画規則（下）。非 0 終了・出力が無い・
    規則を満たさない — 1。Blender が無い — 2。
  - `builtin:threejs-render`: trust 済みの `sitecraft.toml` が要る（無い・未 trust は 2）。dev server が止まっていれば起動し、
    自分が起動したものだけ止める。`serve start` が記録した `url` のページを開き、selector の要素が 30 秒以内に可視に
    ならなければ 1、可視なら 2 秒待って要素のスクリーンショットに描画規則を当てる。Playwright / Chromium が使えない — 2。
  - `command`: D2-trust（上）が一致したときだけ argv を root で実行する。exit 0 → 0、exit 2 → 2、その他・signal → 1。
  - **描画規則**: 四隅の画素の最頻色（同数なら描画率が最小になる候補）を背景とし、どれかのチャネルが背景と 16 を超えて
    違う画素を「描画」とする。描画が 1% 以上なら clean、未満・幅か高さが 0・PNG として読めない — 1。
  - **result record**: 実行のたびに `$SITECRAFT_STATE_DIR/capability-results/<sha256(canonical root)>/<name>.json` に
    `{name, exit, input_hash, sitecraft_version, at}` を一時ファイル＋rename で書く。書けなければ check 自体が 2。
    `input_hash` は `[capabilities.<name>]` の表と入力ファイル（blender: script と `verify.inputs`、threejs:
    `sitecraft.toml`・`web_globs`・`verify.inputs`、command: `verify.inputs`）の内容から決まる。実行中に入力が変わったら 2。
    `command` の `inputs` が何にも一致しない・読めない入力がある — 2。

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
| `SITECRAFT_NODE` | `check pages` と `builtin:threejs-render` の能力が起動する node | `PATH` 上の `node` |
| `SITECRAFT_BLENDER` | `builtin:blender-headless` の能力が起動する Blender（設定時はそれだけを使う） | `PATH` 上の `blender`、次に `/Applications/Blender.app/Contents/MacOS/Blender` |
| `SITECRAFT_NPM` | `check security` が `npm audit` に使う npm | `PATH` 上の `npm` |
| `SITECRAFT_STATE_DIR` | trust store・dev server 状態・gate の記録・能力の result record | `$HOME/.sitecraft` |
| `SITECRAFT_MARKETPLACE` | `decide classify` が見る marketplace.json | なし（未設定は undetermined） |
