# sitecraft-capabilities 裁定補遺（2026-11-03）

`specs/sitecraft-capabilities.toml`（spec id `sitecraft-capabilities`）の正典補遺。
要望文の決定値のうち、ユーザー裁定 **[U]** と実装裁定 **[I]** をここで固定する。
spec の各要求の acceptance は、この文書の値を根拠にする。この文書と要望文が食い違うときは、この文書が優先する。
[I] は 2026-11-03 の実装裁定であり、あとで変えるときはこの文書を改訂し、同じ commit で spec を直す。

既存の正典（変えないもの）: `crates/sitecraft/skills/sitecraft/SKILL.md` の終了コード規約（0 clean / 1 violation /
2 undetermined、2 は合格ではない）と不変条件 1〜5。`crates/sitecraft/README.ja.md` の trust 手順と Stop gate の
挙動（panic barrier、bounded give-up）。この補遺はそれらに**追加**するもので、既存の不変条件は緩めない。

## U. ユーザー裁定（[U]）

- **U1** [U] 置き場所は sitecraft の拡張とする（逐語: 「置き場所は sitecraft の拡張」）。新しい plugin は作らない。
- **U2** [U] 能力の証明は実機証明とする（逐語: 「能力の証明は実機証明（環境プローブだけでは完成としない）」）。
  probe が feasible でも、`check capability` が実機で exit 0 にならなければ完成ではない。
- **U3** [U] 検証テストベッドは `~/src/3d-pipeline-web` とする（逐語: 「検証テストベッドは ~/src/3d-pipeline-web
  （既存の決定記録と scaffold から継続）」）。
- **U4** [U] auth=relevant かつ exposure=irrelevant は矛盾ではなく warning とする（逐語: 「auth=relevant かつ
  exposure=irrelevant は矛盾ではなく warning（decide check は exit 0 のまま警告を表示）」）。
- **U5** [U] ビルドループが止まる状態を検知し、原因がハーネス側か対象アプリ側かを切り分け、ハーネス側なら再現テスト付きで
  harness repo に起票して直す（ユーザー原文: 「止まるようなことはないようにはしてほしいので、そういう状態を検知して
  harnessを修正していってほしい」）。
- **U6** [U] 停滞検知の対象は `/sitecraft` で対象アプリを作っている最中のビルドループに限る。harness repo 自体の開発中に
  止まることは許容する（主セッション経由で伝えられたユーザー裁定の要旨。逐語ではない）。
- **U7** [U] 複数回答の扱いはすべて実装する。選択を求める問いの選択肢は、すべて処理可能なものにする（逐語: 「複数回答のもの
  についてはすべて実装する、選択が必要なものは選択肢は処理可能なものにしてほしい」、2026-11-03）。
  - R7 の「multi の回答は、いずれかの要素が集合に含まれれば真」は、D7 の表に multi の問いを参照する行が無くても評価器に
    実装する。評価器は規則表を引数に取り、本番は閉じた 6 行（D7）を渡す。D7 の表に行は足さない。
  - カタログの select / multi の問いの選択肢は、どれも `sitecraft decide` で記録でき、それを参照する follow-up の活性化が
    働く。コードが拒否・無視する選択肢を置かない。
- **U8** [U] `sitecraft check security` の `npm audit` は、`check pages` が dev コマンドの `--prefix` から解決するのと
  同じパッケージディレクトリで実行する。`--prefix` が無ければ root。そのディレクトリに lockfile が無ければ従来どおり
  UNDETERMINED（exit 2）。解決できない・root 外の `--prefix` も exit 2（2026-11-03、選択肢「Same dir as --prefix
  (Recommended)」を選択。app/ レイアウトで root に lockfile が無く常に exit 2 になり、app/ が監査されない問題への裁定）。

## C. 矛盾の裁定

- **C1** [I] F6 型（domain 問いの回答とカタログ回答の食い違い。例: processing/execution=background と domain の起動経路）は
  `decide check` の規則表の**対象外**とする。規則表はカタログ回答だけを見る（D7）。domain 問いを含む整合性は、
  SKILL の Phase 4 で LLM が回答一覧を読み直し、食い違いを見つけたら AskUserQuestion で確認する手順として扱う。
  `decide check` は domain 問いの整合性について何も判定しない（判定したふりをしない）。
- **C2** [I] `sitecraft status` の state に `slow` を加える。判定式は D11。
- **C3** [I] 要求 7 の本文は U4 に合わせて書き直す（auth/exposure の組は warning であって exit 1 の例ではない）。
- **C4** [I] verify command の trust 単位は D2-trust のとおり「その project の `command` 種の能力の argv の正規化 digest」。
  decisions.toml 全体は hash しない（`decide record` のたびに未 trust に戻るのを避ける）。
- **C5** [I] SKILL の不変条件「空でないディレクトリには scaffold しない」は**変えない**。決定記録（`sitecraft/`）と
  scaffold 先を分けることで F11 を解消する（D6）。scaffold 先は project root 直下の `app/`（テストベッドで観測した配置:
  `~/src/3d-pipeline-web` は `.git/`・`app/`・`sitecraft/`・`sitecraft.toml` の構成）。

## D. 実装裁定（[I]）

### D1 目的

- decisions.toml の `[purpose] text`。前後の空白を除いて空でない文字列なら記録済み。
- 記録は `sitecraft decide purpose --text <要約>`。空白だけの text は記録を変えずに exit 1。
- 未記録なら `decide check` は exit 1 とし、`sitecraft decide purpose --text "<要約>"` の案内を出す。
- `decide next` は purpose を問わない（purpose は SKILL Phase 1 で記録する）。
- 再記録は D8 の history に `area = "purpose"`, `question = "text"` で追記する。

### D2 能力（capability）

- decisions.toml の `[capabilities.<name>]`。name は `^[a-z0-9][a-z0-9-]*$`。集合は開いている（LLM が目的から起こす）。
- キー: `relevance`（`relevant` / `irrelevant`）、`how`（どう作るか、空でない文字列）、`done_when`
  （何をもって動いたとするか、空でない文字列）、`verify`（テーブル）。
- 記録は `sitecraft decide capability --name N --relevance R --how T --done-when T --verify-kind K [種別ごとの引数]`。
  再記録は D8 の history に `area = "capability"`, `question = N` で追記する。
- `verify.kind` は次の 3 つだけ（閉じた集合）:
  - `builtin:blender-headless` — 必須キー `script`（root 相対の Python ファイル）・`glb`（root 相対の出力パス）・
    `render`（root 相対の PNG 出力パス）。
  - `builtin:threejs-render` — 必須キー `page`（`sitecraft.toml` の dev server 上の `/` で始まるパス）。任意キー
    `selector`（既定 `"canvas"`）。
  - `command` — 必須キー `argv`（空でない文字列配列、先頭は空白でない）・`inputs`（空でない glob 配列）。
- 未知の kind・必須キー欠落・型不一致は `decide check` exit 2（記録が使えない）。
- relevance=relevant の能力で how / done_when / verify のどれかが無い → `decide check` exit 1。
- capabilities テーブル自体が無い project は能力なし（純粋な Web アプリ）として扱う。能力が要るかの判断は SKILL Phase 3 の LLM と
  ユーザーの責務で、バイナリは判定しない。
- **D2-trust**: `command` 種の能力の argv は `sitecraft trust` の対象とする。`sitecraft trust` は `sitecraft.toml` の sha256
  （既存）に加えて、`command` 種の全能力について `[name, argv]` の組を name 昇順に並べた JSON 配列の sha256 を、
  trust store の別キー `<canonical root>#capabilities` に記録する。`command` 種の能力が 1 つも無いときはこのキーを
  要求しない。現在の digest と記録が一致しない・記録が無い → その能力の `check capability` は実行せず exit 2
  （`sitecraft trust` の案内付き）。trust store が読めない → exit 2。how / done_when / builtin 種の変更は trust を外さない
  （builtin 種は sitecraft 自身のコードしか実行しない）。LLM は `sitecraft trust` を代わりに実行しない（既存の規約どおり）。

### D3 実現可能性プローブ

- `sitecraft capability probe <name>`。stdout に JSON 1 個 `{"capability":N,"result":"feasible|infeasible|undetermined","reason":…}`。
  終了コードは feasible 0 / infeasible 1 / undetermined 2。時間制限 120 秒、超過は undetermined（reason `timeout`）。
- 未登録の name → exit 2。
- blender の探索順: 環境変数 `SITECRAFT_BLENDER`（空でなければそれだけを使う。指すファイルが無い・実行できない → undetermined）
  → `PATH` 上の `blender` → `/Applications/Blender.app/Contents/MacOS/Blender`。どれも無い → infeasible。
  見つかったら `--background --factory-startup --python <最小スクリプト>` を実行する。最小スクリプトは既定シーンを
  `<state>/probe-tmp/` 配下の GLB に書き出す。終了 0 かつ出力が D4 の GLB 条件を満たす → feasible。
  非 0 終了・出力が条件を満たさない → infeasible。spawn 失敗 → undetermined。
- three.js プローブ: project root から Playwright を解決し（`check pages` と同じ解決方法）、その Chromium を headless で起動して
  `about:blank` で `webgl2`、取れなければ `webgl` の context を取る。どちらか取れれば feasible、どちらも取れなければ infeasible、
  Playwright または Chromium が起動できなければ undetermined。launch 引数は D4 の `check capability` と同じものを使う。
- `command` 種のプローブ: `argv[0]` が `/` を含むなら root 相対または絶対パスのファイルが実行可能か、含まなければ `PATH` で
  解決できるか。可能 → feasible、不可 → infeasible。プローブはコマンドを実行しないので trust を要求しない。
- プローブ結果は記録しない（Ask の材料）。プローブは完成判定に使わない（U2）。

### D4 実機検証（`sitecraft check capability <name>`）

- 終了コード 0 clean / 1 violation / 2 undetermined。時間制限 300 秒、超過は exit 2（reason `timeout`）。
  未登録または relevance=irrelevant の name → exit 2。
- **画素規則**（`builtin:threejs-render` の canvas と `builtin:blender-headless` のレンダ画像の両方に使う）:
  - 背景色 = 画像の 4 隅の画素（RGB。alpha は無視）の最頻値。最頻値が一意でない（4 色すべて違う、または 2 色が 2 個ずつ）
    ときは、候補色それぞれについて下の描画率を計算し、**最小になる候補**を背景色とする（制限側）。
  - 描画画素 = R/G/B のいずれかのチャネルで背景色との差の絶対値が 16 を超える画素。
  - 描画率 = 描画画素数 / 全画素数。1% 以上で clean、1% 未満は violation。幅か高さが 0 は violation。
- **GLB 条件**: ファイル先頭 4 バイトが `glTF`、続く uint32 LE の version が 2、その次の uint32 LE の length がファイルサイズと一致。
  満たさない（0 バイトを含む）→ violation。
- `builtin:blender-headless`: 実行前に `glb` と `render` の既存ファイルを削除する（古い出力で通さない）。D3 と同じ探索で blender を
  決め（見つからない → exit 2）、cwd = root で `<blender> --background --factory-startup --python <script> -- --glb <絶対パス>
  --render <絶対パス>` を実行する。非 0 終了 → violation。終了 0 で `glb` が GLB 条件を満たし、かつ `render` が PNG として読めて
  画素規則で clean なら clean。出力ファイルが無い・PNG として読めない → violation。読み取りの IO 失敗（権限など）→ exit 2。
  script が無い → exit 2。
- `builtin:threejs-render`: trust 済み `sitecraft.toml` が要る（無い・未 trust → exit 2）。dev server が起動していなければ
  `serve start` と同じ手順で起動し、終了時に自分が起動したものだけを止める。`http://127.0.0.1:<port><page>` を Playwright
  Chromium（headless）で開き、load の後 `selector` が可視になるまで最大 30 秒待つ（待てない → violation）。さらに 2000 ms 待ってから
  その要素のスクリーンショット（PNG）を撮り、画素規則で判定する。console error は判定しない（`check pages` の責務）。
  Playwright・Chromium が起動できない → exit 2。
- `command`: D2-trust を満たさなければ実行しない（exit 2）。cwd = root で argv を実行する。コマンドの終了コード 0 → clean、
  2 → undetermined、それ以外の非 0・signal 終了 → violation。spawn 失敗 → exit 2。
- 結果記録は D5。

### D5 結果記録と Stop gate

- `check capability` は終了ごとに `<state>/capability-results/<root-key>/<name>.json` に
  `{"name","exit","input_hash","sitecraft_version","at"}` を一時ファイル経由の rename で書く。
  `<root-key>` = canonical な project root パス文字列の sha256 hex。書けない → `check capability` 自体を exit 2 にする。
- **入力ハッシュの範囲**（S4 の裁定）: 次の集合を root 相対パス昇順に並べ、各要素を `(パス, 内容の sha256)` として連結し、
  その前に `[capabilities.<name>]` テーブルの正規化 JSON（キー昇順）を置いたものの sha256。
  - `builtin:blender-headless`: `script` のファイル ＋ `verify.inputs`（任意キー、glob 配列）に一致するファイル。
  - `builtin:threejs-render`: `sitecraft.toml` ＋ その `web_globs` に一致するファイル ＋ `verify.inputs` に一致するファイル。
  - `command`: `verify.inputs` に一致するファイル（一致 0 件 → exit 2。空集合は判定不能）。
  - 読めないファイルがある → exit 2。
- Stop gate は能力を**実行しない**。relevance=relevant の全能力について、記録があり、`exit == 0`、`sitecraft_version` が
  現在の sitecraft と一致し、`input_hash` が現在の入力から再計算した値と一致するときだけその能力を clean とする。
  記録が無い・exit ≠ 0・古い・読めない・再計算できない → block。block 理由には能力名と
  `sitecraft check capability <name>` の案内を含める。既存の検査とは `Verdict::worst_of` で合成し、既存の bounded give-up に従う。
  decisions.toml に relevant な能力が無ければこの項は何も追加しない。

### D6 repo 作成（`sitecraft init <dir>`）

- `<dir>` が存在しない、または空のディレクトリ → ディレクトリを作り（必要なら）、`git init` し、`<dir>/sitecraft/` を作る。exit 0。
- `<dir>` が空でない・ファイルである → 何も変えずに exit 1。
- `<dir>` の親が別の git work tree の中にある → 何も変えずに exit 1（入れ子 repo の事故を防ぐ）。
- `git` が無い・`git init` が非 0・30 秒の時間制限を超えた・IO 失敗 → exit 2。
- scaffold 先は `<root>/app/`。SKILL の「空でないディレクトリには scaffold しない」は `app/` に対して適用する（`app/` が存在して
  空でなければ拒否）。決定記録は `<root>/sitecraft/decisions.toml`、`sitecraft.toml` は `<root>` 直下のまま。
  SKILL の `sitecraft.toml` の例は `npm --prefix app …`・`build_dir = "app/dist"`・`web_globs = ["app/src/**", …]` の形にする。
- 既存 project（例: U3 のテストベッド）は init を経ずに継続してよい。

### D7 決定の整合性規則（閉じた表）

規則は `crates/sitecraft/src/decide/` 内のコード上の表で、**次の 6 行がすべて**（追加・削除はこの文書の改訂を要する）。
対象はカタログ回答だけで、domain 問いは対象外（C1）。各規則に「発火する例」と「発火しない例」のテストを付ける。

| id | 条件 | 判定 | 表示文の要旨 |
|---|---|---|---|
| W1 | auth.relevance=relevant かつ exposure.relevance=irrelevant | warning | 公開範囲が未定のままログインを作ろうとしている |
| V1 | exposure.hosting=static かつ processing.execution ∈ {in-request, background} | violation | 静的ホスティングではサーバー側処理を実行できない |
| V2 | exposure.hosting=static かつ storage.file-storage=local-disk | violation | 静的ホスティングには書き込めるローカルディスクが無い |
| V3 | users-roles.user-model ∈ {same-rights, multi-role} かつ（auth.relevance=irrelevant または auth.needs-login=no） | violation | 利用者を区別するのにログインが無い |
| V4 | admin.admin-ui=yes かつ（auth.relevance=irrelevant または auth.needs-login=no） | violation | 管理画面があるのにログインが無い |
| V5 | workflow.on-failure ∈ {notify, both} かつ notification.relevance=irrelevant | violation | 失敗を知らせるのに通知が無い |

- 評価: 条件の各項は記録された値だけを見る。記録が無い回答（領域が irrelevant、follow-up が非活性、未回答）を参照する項は偽。
  未回答は既存どおり別途 exit 1 になる。multi の回答は「いずれかの要素が集合に含まれれば真」。
- 出力: violation は既存の未回答一覧と同じ出力に `<id>: <表示文>` と関与した (area, question, answer) を並べ、exit 1。
  warning は `warning: <id>: <表示文>` を stderr に出し、終了コードを変えない（U4）。
- 要望文の例「notification=relevant かつ workflow/on-failure が無い」は採らない（workflow=irrelevant なら on-failure は問われず、
  矛盾ではない）。

### D8 回答の表現力と履歴

- 質問定義の `multi = true` で複数選択を許可する。カタログで multi にするのは `notification.channel` だけ。domain 問いは
  `decide add-question --multi` で multi にする。
- multi の問いには `--answer` を繰り返して配列で保存する（1 個でも配列）。要素は空でなく重複なし、choices があれば各要素が
  choices に含まれる。違反は既存の不正回答と同じく記録を変えずに失敗終了。multi でない問いに `--answer` を 2 個以上 → 同じく失敗。
- 読み込み: multi の問いに旧形式の文字列が入っていれば 1 要素の配列として読む。multi でない問いに配列が入っていれば
  記録が使えない（exit 2）。
- follow-up の活性条件は any-of: 条件の問いの回答（multi なら要素のいずれか）が条件集合に含まれれば活性。
  例: channel=[web-push, email] なら email-sender を問う。
- 補足は `--note <text>` で、カタログ回答は `[areas.<a>.notes] <q> = text`、domain 問いは `[[questions]]` の `note` に保存する。
  `--note` を付けない再記録は既存の note を残す。
- 履歴: 値（回答・relevance・note・purpose・capability）が変わる記録ごとに `[[history]]` に `{area, question, old, new, at}` を追記する
  （note の変更は `question = "<q>#note"`、`at` は UTC の RFC 3339）。同じ値の再記録は history を増やさず changed も出さない。
  変わったときは stdout に `changed: <old> -> <new>` を 1 行出す。follow-up の剪定で消える回答も `{…, new = "", pruned = true}` で
  history に残す。

### D9 classify の目的適合

- `dedicated-harness` の出力 JSON に `description`（marketplace.json の当該エントリの description。existing=false なら null）と
  `reuse` を加える。`reuse` は `"unconfirmed"`（existing=true で適合の記録なし）/ `"confirmed"` / `"rejected"` /
  `"not-applicable"`（existing=false または kind=simple）。
- `--confirm-fit yes|no` は existing=true のときだけ受け付け（それ以外は失敗終了）、`[features.<F>] fit` に記録する。
  yes → confirmed、no → rejected。
- SKILL: description を AskUserQuestion でユーザーに見せ、ユーザーの回答だけを `--confirm-fit` に記録する（LLM が自答しない）。
  `confirmed` のときだけ既存 plugin を使う。`unconfirmed` を再利用許可と読まない。`rejected` は existing=false と同じく新規 plugin
  （別名）を作る経路に進む。SKILL の「existing: true ならその plugin を使う」はこの規則に改める。
- simple / dedicated-harness の境界（SKILL に明記）: 対象アプリのプロセス内で動き、アプリの利用者のための機能（ジョブ実行・
  予算制御・通知送信・認証など）は **simple**。Claude Code のセッション・ツール呼び出し・ファイル編集を判定するもの（hook・gate・
  検査ハーネス）だけが **dedicated-harness**。

### D10 フロント＋バックエンド構成

- `sitecraft.toml` に任意キー `api_paths`（`/` で始まる文字列の配列。既定は空）を追加する（trust 対象は既存どおりファイル全体）。
- `check pages` は各 `api_paths` に `http://127.0.0.1:<port>` から GET する（リダイレクトは追わない）。2xx 以外・接続失敗は
  violation、1 パス 10 秒の時間制限超過は exit 2。
- SKILL に Vite + Hono（`@hono/vite-dev-server`）で 1 つの dev command でフロントと API を起動する手順を持つ
  （テストベッド `app/` の `vite.config.ts` と `server/app.ts` の構成）。

### D11 時間制限・停滞検知・status

- **時間制限（閉じた一覧）**: probe 120 秒 / check capability 300 秒 / `sitecraft init` の git 30 秒。既存の制限は変えない:
  `check pages` は既存の `deadline_for`（30 + 45×ページ数 秒、上限 480 秒、＋30 秒の余裕）、`npm audit` 300 秒、
  `serve start` の ready 待ちは `ready_timeout_secs`、D10 の api_paths は 1 パス 10 秒。超過は exit 2（reason `timeout`）。
- **check 履歴**: `decide check`・`check pages`・`check security`・`check capability <name>` は、CLI でも gate 経由でも、終了ごとに
  `<state>/check-history/<root-key>.jsonl` に `{"name","exit","fingerprint","elapsed_ms","timed_out","at"}` を 1 行追記する。
  name は `decide` / `pages` / `security` / `capability:<name>`。追記できなければその検査を exit 2 にする。
- **失敗の指紋**: exit ≠ 0 のとき、その検査が出力した stdout と stderr を連結し、(1) RFC 3339 / ISO 8601 の時刻 → `<time>`、
  (2) `/` で始まり空白までの絶対パス → `<path>`、(3) 数字の連続 → `0` の順に置換した文字列の sha256 hex。時間切れは
  `sha256("timeout")`。exit 0 のときは null。
- **`sitecraft status`**: stdout に JSON `{"checks":[{"name","last_exit","consecutive_same_failure","elapsed_ms","state"}]}`。
  `consecutive_same_failure` = 履歴の末尾から、exit ≠ 0 かつ指紋が最新と同じ行が連続する数。state は次の優先順で 1 つ:
  1. `waiting-human` — その検査が trust を要し（`pages`、`capability:` の threejs-render 種と command 種）、status 実行時点で
     `sitecraft.toml` または D2-trust の digest が未 trust。
  2. `stuck` — `consecutive_same_failure >= 3`。
  3. `failing` — 最新の exit ≠ 0。
  4. `slow` — 最新の exit = 0 かつ `elapsed_ms` がその検査の時間制限の 50% 以上（`decide` は制限が無いので slow にならない）。
  5. `ok`。
- status の終了コード: 全検査が ok / slow → 0。いずれかが failing / stuck / waiting-human → 1。履歴が無い・読めない・パースできない行が
  ある・state dir が決まらない → 2（`{"checks":[]}` を「問題なし」と読ませない）。
- stuckguard（ツール呼び出しの反復検知）とは補完関係で、再実装しない。

### D12 停滞時の手順（SKILL）

- `stuck` を見たら同じ検査の再実行ループを止め、原因を次の閉じた基準で切り分けて記録してから動く:
  - **ハーネス側** — (a) sitecraft が panic・内部エラーで終わる、(b) 検査の判定が直接の観測と食い違う（例: 手で開いた
    GLB・画面は正常なのに violation）、(c) SKILL の手順どおりのコマンドが存在しない・引数が拒否される、(d) 検査が対象アプリの
    状態に関係なく同じ指紋を出す（アプリを変えても指紋が変わらない）。
  - **対象アプリ側** — 上のどれにも当たらない。アプリを直して再実行する。同じ指紋で更に 3 回 stuck が続くならユーザーに Ask する。
- ハーネス側なら: harness repo（`SITECRAFT_MARKETPLACE` の marketplace.json がある repo）の worktree で、その停滞を再現する
  失敗テスト（RED を観測したもの）を添えて `backlog add` で起票し、修正・rollout してから対象アプリの作業を再開する。
- `waiting-human` は停滞ではない。どのファイルに `sitecraft trust` が要るかを示してユーザーに Ask し、待ち続けない。
- U6 により、harness repo 自体の開発中の停止はこの手順の対象外。

### D13 実機証明の完了条件（U2・U3）

次がすべて `~/src/3d-pipeline-web` で観測されたときに spec を完成とする:

1. `[purpose]` が記録され、`sitecraft decide check` が exit 0（W1 の warning が表示されることも確認する）。
2. `blender-headless`（`builtin:blender-headless`）と `threejs-render`（`builtin:threejs-render`）が relevance=relevant で登録され、
   両方の `sitecraft capability probe` が feasible。
3. 両方の `sitecraft check capability` がこの Mac の実 Blender（`/Applications/Blender.app`）と Playwright Chromium で exit 0。
4. その状態で Stop gate が block しない（能力の記録が新しいことを gate が確認する）。

GLB→VRM 変換（`command` 種）は完了条件に含めない（[U] 2026-11-03 にユーザーが AskUserQuestion でこの範囲を確認）。`command` 種はテスト内の fixture で検証する。テストベッド側に必要な
blender スクリプトと three.js の表示ページは、テストベッドの repo で `/sitecraft` の手順に従って作る（harness repo には置かない）。
