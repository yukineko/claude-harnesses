---
name: sitecraft
description: サイト／Web アプリのプロトタイプを作るスキル。作りたいもの（目的）を聞き、目的から必要な設計判断を洗い出して全部ユーザーに決めてもらい（sitecraft decide）、記録が揃ってから scaffold（既定 Vite）→ 実装 → E2E（webqa-gen）→ sitecraft check で検査し、すべて exit 0 になってから完成を報告する。
argument-hint: '[作りたいもの]'
---

`/sitecraft [作りたいもの]` で、**目的のヒアリング → 設計判断の記録 → 計画と切り出し → scaffold → 実装 → E2E → 検査 → 完成報告** を回す。
判定（どの判断が要るか・どう実装するか）は LLM、記録と検査は `sitecraft` バイナリ（決定論）が担う。

## 終了コード規約（全サブコマンド共通・読み違えない）

| code | 意味 |
|------|------|
| `0` | clean — 検査が最後まで走り、何も見つからなかった |
| `1` | violation — 直すべきものが見つかった |
| `2` | undetermined — 検査が結論まで走れなかった（IO 失敗・パース不能・ツール欠落・空の検査対象・未 trust の設定 …） |

**`2` は合格ではない。** `1` と同じく止まって原因を直す。`2` を「たぶん大丈夫」と読んで先へ進むことは禁止。

## 不変条件（外さない）

1. **未回答のまま scaffold へ進まない。** 目的が `sitecraft decide purpose` で記録され、`sitecraft decide next` が `{"status":"done"}` を返し、かつ `sitecraft decide check` が exit 0 になるまで、scaffold・実装に入らない。
2. **仮の既定値で埋めない。** ユーザーが答えていない問いに LLM が「とりあえず」「一般的には」で答えを入れて `decide record` してはならない。わからない問いは AskUserQuestion で聞く。「関係なし（irrelevant）」も**ユーザーの判断を経た記録**であって、問いを省略する手段ではない。
3. **テスト作者と実装者を別 subagent に分ける。** 自分の実装を検証するテストを自分で書かない（CLAUDE.md §2(a)）。
4. **E2E は既存の `webqa-gen` スキルに委ねる。** Playwright のテスト生成をこのスキル内で再実装しない。
5. **完成報告は全検査 exit 0 の後だけ。** `sitecraft check pages`・`sitecraft check security`・`sitecraft decide check`、および relevant な能力ごとの `sitecraft check capability <name>` がすべて exit 0 になってからプロトタイプ完成を報告する。exit 2 は合格扱いしない。

## 手順

### Phase 1 — 目的を聞く

1. 作りたいもの（目的）を **AskUserQuestion** で聞く。引数で渡されていても、誰が何のために使うか・最低限動いてほしいことを確認する。
2. 目的を 1〜3 文に要約してユーザーに確認する。以降の判定はこの要約を根拠にする。
3. 確認した要約を記録する（`decide next` は目的を問わないので、ここで必ず記録する。未記録のままだと `decide check` は exit 1）:
   ```sh
   sitecraft decide purpose --text "<要約>"
   ```
   空白だけの `--text` は exit 1 で記録されない。目的が変わったら同じコマンドで再記録する（旧値は `[[history]]` に残る）。

### Phase 2 — カタログ 12 領域の関連判定

カタログの 12 領域（`auth` / `users-roles` / `admin` / `storage` / `data-analytics` / `web-analytics` / `processing` / `workflow` / `notification` / `ai` / `formats` / `exposure`）**それぞれについて**、目的との関連を LLM が判定し、根拠を添えてユーザーに確認したうえで記録する。

```sh
sitecraft decide record --area <area> --question relevance --answer relevant
sitecraft decide record --area <area> --question relevance --answer irrelevant
```

- **関係なし（irrelevant）も必ず記録する。** 記録されていない領域は「未判定」であって「関係なし」ではない。
- 判定に迷う領域は relevant 側に倒してユーザーに聞く。

### Phase 3 — カタログに無いドメイン固有の問いを起こす

目的から、カタログでは拾えないドメイン固有の問いを起こし、記録する。例:

- VRM を扱うサイトなら「VRM 以外の 3D 形式（glTF / FBX 等）をどうするか」
- 「データやログの解析は要るか」「既存データの取り込み元はどこか」

```sh
sitecraft decide add-question --id <id> --text "<問い>" [--choice <選択肢> ...] [--multi]
```

`--choice` を付けない問いは自由記述。複数選んでよい問いには `--multi` を付ける（回答は配列で記録される）。

**Web アプリの外の能力（capability）を記録する。** 目的が Web アプリ以外の成果物を要するとき（Blender で 3D モデルを書き出す・three.js で描画する・変換 CLI を動かす等）、能力ごとに `[capabilities.<name>]` を記録する（`<name>` は `^[a-z0-9][a-z0-9-]*$`）。関連（relevant / irrelevant）の判定はカタログ領域と同じく根拠を添えてユーザーに確認する。relevant な能力の **how（どう実現するか）と done_when（何をもって完成とするか）は LLM が決めない。** 案を示したうえで **AskUserQuestion** でユーザーに聞き、答えだけを記録する。verify（完成を機械判定する方法）は次の 3 種類だけ:

```sh
# Blender を headless で動かし GLB と render PNG を書かせる（script は root 相対）
sitecraft decide capability --name <name> --relevance relevant --how "<how>" --done-when "<done_when>" \
  --verify-kind builtin:blender-headless --script <script.py> --glb <out.glb> --render <out.png> [--input <glob> ...]
# dev server のページを headless Chromium で開き、selector（既定 canvas）の描画を確かめる
sitecraft decide capability --name <name> --relevance relevant --how "<how>" --done-when "<done_when>" \
  --verify-kind builtin:threejs-render --page /<page>.html [--selector <css>] [--input <glob> ...]
# 任意のコマンド（exit 0 = clean / 2 = undetermined / それ以外 = violation）
sitecraft decide capability --name <name> --relevance relevant --how "<how>" --done-when "<done_when>" \
  --verify-kind command --argv <prog> [--argv <arg> ...] --input <glob> [--input <glob> ...]
# 関係なし
sitecraft decide capability --name <name> --relevance irrelevant
```

relevant なのに how / done_when / verify が欠けた能力は `decide check` が exit 1 で列挙する。キーの誤り・型違い・未知の verify kind・root の外を指すパスは exit 2（記録は変わらない）。変更は `[[history]]`（`area = "capability"`）に残る。

`command` の verify は任意のプログラムを実行するので、その `argv` は `sitecraft trust` が人間の承認として記録する（`sitecraft.toml` と同じ手順。argv を変えたら trust し直すまで `check capability` は exit 2 で何も実行しない）。LLM が人間の代わりに `sitecraft trust` を実行しない。

**記録したら、その場で動くかを確かめる（probe）。** relevant な能力ごとに `sitecraft capability probe <name>` を実行する。stdout に `{"capability","result","reason"}` の JSON が 1 行出る（exit 0 `feasible` / 1 `infeasible` / 2 `undetermined`、120 秒で打ち切り `timeout`）。probe は何も書かない。結果（`result` と `reason`）を **AskUserQuestion** でユーザーに示し、infeasible / undetermined なら「環境を整える（Blender の導入・`SITECRAFT_BLENDER` の設定・Playwright の導入）か、how / verify を変えるか」を聞いて決めてもらう。

probe の `feasible` は「この環境で動かせる」という事前確認であって完成ではない。feasible を根拠に完成と報告しない（完成の根拠は Phase 10 の `sitecraft check capability` の exit 0 だけ）。

### Phase 4 — 全問に答えてもらう（Ask ループ）

次のループを `decide next` が done を返すまで回す。

1. `sitecraft decide next` を実行する。出力は JSON:
   - `{"status":"ask","area":…,"question_id":…,"text":…,"choices":[…],"multi":bool}` — 次の未回答の問い
   - `{"status":"done"}` — すべて回答済み
   - exit 2 — 記録が壊れている・読めない。メッセージの fix に従って直す（握りつぶして先へ進まない）
2. `ask` なら、その `text` と `choices` を **AskUserQuestion** でユーザーに聞く（`choices` が空なら自由記述。`multi: true` なら複数選択で聞く）。
3. 回答を記録する:
   ```sh
   sitecraft decide record --area <area> --question <question_id> --answer <回答> [--answer <回答2> ...] [--note "<補足>"]
   ```
   ドメイン問い（Phase 3）は `--area domain --question <id>` で記録する。`--answer` を複数渡せるのは `multi: true` の問いだけ（それ以外は失敗終了）。
   ユーザーが回答に添えた条件・理由は `--note` で残す（`--note` を付けない再記録は既存の補足を残す）。
   値が変わった再記録では stdout に `changed: <旧> -> <新>` が出て `[[history]]` に残る。その変更で問われなくなった follow-up の回答も `pruned = true` として history に残る。
4. 1. に戻る。

done が返ったら `sitecraft decide check` を実行し、**exit 0 を確認する**。exit 1 なら列挙された未回答・未記録の目的（`sitecraft decide purpose --text` の案内）・整合性規則の違反（`V1`〜`V5` の `<id>: <表示文>` と関与した回答）を、ユーザーに AskUserQuestion で確認して記録し直す。exit 2 なら記録の不整合を直す。exit 0 になるまで Phase 5 以降へ進まない。stderr の `warning: W1: …` は終了コードを変えない警告だが、黙って流さずユーザーに示して意図どおりか確認する。

`decide check` の規則表はカタログの回答だけを見る。ドメイン問いの回答とカタログ回答の食い違い（例: processing の execution=background なのに、ドメイン問いでは起動経路を利用者の操作と答えている）はバイナリは判定しないので、done と exit 0 の後に LLM が回答一覧（ドメイン問い・`--note` の補足を含む `sitecraft/decisions.toml` 全体）を読み直し、食い違いを見つけたら AskUserQuestion でユーザーに確認して、どちらかを記録し直す。食い違いを LLM の判断で解消したことにしない。

### Phase 5 — 計画と切り出し（simple / dedicated-harness）

記録した判断から実装する機能を列挙し、機能ごとに分類して記録する。

```sh
# このサイトの中で素直に実装できるもの
sitecraft decide classify --feature <name> --kind simple
# 独立した検査・ゲート・状態管理が要るもの（専用 harness 化）
SITECRAFT_MARKETPLACE=<harness repo>/.claude-plugin/marketplace.json \
  sitecraft decide classify --feature <name> --kind dedicated-harness [--harness <plugin>]
```

simple と dedicated-harness の境界: 対象アプリのプロセス内で動き、アプリの利用者のための機能（ジョブ実行・予算制御・通知送信・認証など）は **simple**（アプリの中で実装する）。

Claude Code のセッション・ツール呼び出し・ファイル編集を判定するもの（hook・gate・検査ハーネス）だけが **dedicated-harness**。アプリの機能が「独立した検査が要りそう」に見えても、判定する対象が Claude Code の作業でなければ simple に分類する。

`dedicated-harness` の出力 JSON の `existing`・`description`・`reuse` を見る:

- `existing: true` — 同名の plugin が marketplace にある。ただし名前が同じでも目的に合うとは限らない。出力の `description`（marketplace.json の当該エントリの説明）を AskUserQuestion でユーザーに示し、「この plugin はこの目的に合うか」を聞いて、ユーザーの回答だけを `--confirm-fit yes|no` で記録する（LLM が自答して `--confirm-fit` を渡さない）:
  ```sh
  SITECRAFT_MARKETPLACE=<harness repo>/.claude-plugin/marketplace.json \
    sitecraft decide classify --feature <name> --kind dedicated-harness [--harness <plugin>] --confirm-fit yes
  ```

`reuse: confirmed`（ユーザーが yes と答えた）のときだけ既存 plugin を使う（README を読んで導入し、再実装しない）。

`reuse: unconfirmed`（適合の記録がまだ無い）は再利用の許可ではない。再利用してよいと読まず、上の AskUserQuestion で確認するまで先へ進まない。

`reuse: rejected`（ユーザーが no と答えた）は `existing: false` と同じく新規 plugin を作る経路に進む。既存と名前が衝突するので、新規 plugin は別名（`--harness <別名>` で classify し直す）で作る。

- `existing: false`（`reuse: not-applicable`）— 無い。harness repo に新規 plugin を作る（`--confirm-fit` は受け付けられない）。雛形（`crates/<plugin>/`）:
  ```
  crates/<plugin>/
    Cargo.toml                    # [package] version = "0.1.0"
    .claude-plugin/plugin.json    # name / version / description（version は Cargo.toml と一致）
    hooks/hooks.json              # 判定を持つなら Stop hook（fail-closed）
    bin/<plugin>                  # launcher（100755・バイナリ不在は block）
    skills/<plugin>/SKILL.md
    src/main.rs                   # 判定は harness_core::verdict::Verdict（三値）
    README.ja.md
  ```
  あわせて `.claude-plugin/marketplace.json` にエントリを追加し、3 ファイルの version を lockstep させる。新規 plugin の作成は condukt で別タスクとして回す。
- exit 2（`SITECRAFT_MARKETPLACE` 未設定・読めない）— 「既存が無い」と読まない。パスを直して再実行する。

### Phase 6 — scaffold（既定 Vite、scaffold 先は `<root>/app/`）

- 新しい repo は `sitecraft init <dir>` で作る。`<dir>` が存在しないか空のときだけ `git init` と `<dir>/sitecraft/` を作って exit 0。空でない・ファイル・別の git work tree の中なら exit 1 で何も変えない。git が無い・`git init` 失敗・git が 30 秒を超えた・IO 失敗は exit 2（合格ではない。原因を直して再実行）。既存の project は init を経ずに続けてよい。
- scaffold 先は **`<root>/app/`**。決定記録 `<root>/sitecraft/decisions.toml` と `sitecraft.toml`（`<root>` 直下）は `app/` の外に置く。`<root>/sitecraft/decisions.toml` がある root でも、空の（または存在しない）`app/` への scaffold は拒否しない。
- 既定は **Vite**（`<root>` で `npm create vite@latest app`）。Next.js 等は**ユーザーが選んだ場合だけ**使う（選択制。AskUserQuestion で確認する）。
- **空でないディレクトリには scaffold しない。** この規則は scaffold 先の `app/` に適用する: `app/` が存在して空でなければ拒否し、別のディレクトリをユーザーに聞く（上書き・マージはしない）。

**フロントと API を 1 つの dev command で起動する（Vite + Hono）。** API が要るサイトでは、Hono のアプリを `@hono/vite-dev-server` で Vite の dev server に載せ、`npm --prefix app run dev` の 1 コマンドでフロントと API を同じ port で起動する（別プロセスの API server を立てない）。構成は `app/` の中に次の 2 ファイル: `app/vite.config.ts` に `devServer({ entry: 'server/app.ts', … })` を足し、`app/server/app.ts` で Hono アプリを `export default` する（API は `/api/` 以下）。`app/` で `npm i hono` と `npm i -D @hono/vite-dev-server` を入れる。確認したい API のパスは `sitecraft.toml` の `api_paths` に書く（Phase 7）。

```ts
// app/vite.config.ts
import { defineConfig } from 'vite'
import devServer, { defaultOptions } from '@hono/vite-dev-server'

export default defineConfig({
  plugins: [
    devServer({
      entry: 'server/app.ts',
      // /api/ 以外は Vite（フロント）が返す
      exclude: [/^(?!\/api\/).*/, ...defaultOptions.exclude],
    }),
  ],
})
```

```ts
// app/server/app.ts
import { Hono } from 'hono'

const app = new Hono()
app.get('/api/health', (c) => c.json({ ok: true }))

export default app
```

### Phase 7 — sitecraft.toml と trust

`<root>` 直下（`app/` の外）に `sitecraft.toml` を書く（キーはすべてトップレベル。アプリは `app/` にあるので `command` は `npm --prefix app …`）:

```toml
command = ["npm", "--prefix", "app", "run", "dev", "--", "--port", "5173", "--strictPort"]
port = 5173
ready_path = "/"
ready_timeout_secs = 60
pages = ["/"]
build_dir = "app/dist"
web_globs = ["app/src/**", "app/index.html", "app/public/**", "app/server/**"]
# 任意: check pages が GET で 2xx を確かめる API のパス（/ で始まる。リダイレクトは追わない）
api_paths = ["/api/health"]
```

- `check pages` は `command` の `npm --prefix app` から `app/` を web-app package とみなし、そこ（と祖先）の `node_modules` から Playwright を解決する。Playwright は `app/` に入れる（`npm --prefix app i -D playwright && (cd app && npx playwright install chromium)`）。
- `api_paths` の各パスは pages と同じ宛先（`--base-url`、無ければ `serve start` が記録した `url` ＝ 応答を確認した loopback アドレスで `[::1]` のこともある、サーバの記録が無ければ `http://127.0.0.1:<port>`。Stop gate も `serve start` の `url` を使う）に GET され、2xx 以外（3xx を含む）・接続失敗は exit 1、1 パス 10 秒を超えたら exit 2。`/` で始まらない要素は設定エラー（exit 2）。

書いたら **`sitecraft trust` は人間に実行してもらう。** `command` は Stop gate と `sitecraft serve start` が実行するコマンドなので、内容を人間が確認して承認する必要がある。LLM が代わりに `sitecraft trust` を実行しない。未 trust の設定では `serve start` も gate も止まる（exit 2 / block）。`sitecraft trust` は同時に `sitecraft/decisions.toml` の `command` 能力の argv も承認として記録する（`command` 能力が無ければその記録を消す）。`decisions.toml` が読めない・`[capabilities]` が壊れているときは何も trust せず exit 2。

### Phase 8 — 実装

- 機能ごとに、**テスト作者 subagent** と **実装者 subagent** を分ける。テスト作者が先に RED を観測したテストを書き、実装者はそのテストを変更せずに GREEN にする。
- 開発中の動作確認は `sitecraft serve start` / `sitecraft serve status` / `sitecraft serve stop`。

### Phase 9 — E2E（webqa-gen に委ねる）

E2E テストは既存の **`/webqa-gen`** スキルで生成する（Playwright の spec 生成・RED 観測・弱体化検査はそちらの責務。ここで再実装しない）。webqa-gen も実装者とは別の subagent で回す。

### Phase 10 — 検査

ビルドしてから、次をすべて実行する:

```sh
npm --prefix app run build
sitecraft serve start
sitecraft check pages
sitecraft serve stop
sitecraft check security
sitecraft decide check
sitecraft check capability <name>   # relevant な能力ごとに
```

- `check pages` — 設定した pages を Playwright で開き、console error・page error・asset の HTTP エラー・壊れたリンクを検出する。`--base-url` を付けなければ、`sitecraft serve start` が記録した `url`（`sitecraft serve status` が running と報告するもの。macOS の Vite のように `[::1]` しか listen しない dev server なら `http://[::1]:<port>`）を pages と `api_paths` の宛先にする。サーバの記録が無い（stopped）なら `http://127.0.0.1:<port>`。serve state が判定できない（state file が読めない・壊れている・記録した group が死んでいる）ときは node を起動せず exit 2 になるので、`sitecraft serve status` で原因を見て `sitecraft serve stop` → `sitecraft serve start` してから再実行する。`--base-url` を明示したときは常にそれが優先する。
- `check security` — build 出力の秘密情報（伏字で表示）・`.env` の値の混入・危険な HTML sink・`npm audit` の high/critical を検出する。`npm audit` は `check pages` と同じ package ディレクトリ（`command` が `npm --prefix app …` なら `app/`、`--prefix` が無ければ root）で走るので、lockfile はそこに置く（`npm --prefix app i --package-lock-only`）。そこに lockfile が無い・`--prefix` が root 内のディレクトリに解決できないときは exit 2。
- `check capability <name>` — 能力の verify を実行して判定する（300 秒で打ち切り exit 2）。`builtin:blender-headless` は古い GLB / render を消してから Blender を走らせ、GLB ヘッダ（`glTF`・version 2・長さ一致）と render の描画（四隅の最頻色を背景とし、どれかのチャネルが 16 を超えて違う画素が 1% 以上）を確かめる。`builtin:threejs-render` は trust 済みの `sitecraft.toml` の dev server（止まっていれば起動し、自分で起動したものだけ止める）でページを開き、selector の要素のスクリーンショットに同じ描画規則を当てる。`command` は trust 済みの argv を root で実行する。結果は state dir の result record に残り、Stop gate はそれを読む（gate 自身は能力を実行しない）。入力（script・`verify.inputs`・threejs では `sitecraft.toml` と `web_globs`）や `[capabilities.<name>]` を変えたら再実行する。
- exit 1 は指摘を直して再実行。exit 2 は検査が走れなかった原因（未ビルド・Playwright 未導入・lockfile 無し・Blender 不在・未 trust 等）を直して再実行。

### 停滞の検知と対処（sitecraft status）

この手順の対象は `/sitecraft` で対象アプリを作っている最中のビルドループ（Phase 8〜10 の実装・検査の繰り返し）に限る。harness repo 自体の開発中に止まることはこの手順の対象外。

`sitecraft decide check`・`sitecraft check pages`・`sitecraft check security`・`sitecraft check capability <name>` は、CLI でも Stop gate 経由でも、1 回ごとに state dir の check 履歴へ 1 行（名前・exit・失敗の指紋・所要時間・時間切れか・時刻）を追記する（追記できなければその検査は exit 2）。失敗の指紋は出力の時刻・絶対パス・数字を均した sha256 なので、同じ原因の失敗は同じ指紋になる。検査を再実行する前に `sitecraft status` を実行し、stdout の `{"checks":[{name, last_exit, consecutive_same_failure, elapsed_ms, state}]}` の `state` を読む（exit 0 = 全検査が ok / slow、1 = failing / stuck / waiting-human がある、2 = 履歴が無い・読めない・壊れている、または判定できない。2 は「問題なし」ではない）。

- `ok` / `slow` — 先へ進む（`slow` は時間制限の半分以上かかった成功。止まってはいないが、遅さの原因はユーザーに示す）。
- `failing` — 通常どおり出力を読んで直し、再実行する。
- `stuck`（同じ指紋の失敗が 3 回以上連続）— 同じ検査の再実行ループをそこで止める。直す前に、原因を次の閉じた基準で切り分けて、どれに当たったかを記録する。

**ハーネス側**の基準: (a) sitecraft が panic・内部エラーで終わる。(b) 検査の判定が直接の観測と食い違う（例: 手で開いた GLB・画面は正常なのに violation）。(c) この SKILL の手順どおりのコマンドが存在しない・引数が拒否される。(d) 検査が対象アプリの状態に関係なく同じ指紋を出す（アプリを変えても指紋が変わらない）。(a)〜(d) のどれにも当たらなければ**対象アプリ側**。

ハーネス側なら、harness repo（`SITECRAFT_MARKETPLACE` の marketplace.json がある repo）の worktree で、その停滞を再現する失敗テストを書いて RED を観測し、そのテストを添えて `backlog add` で起票する。修正して rollout してから対象アプリの作業を再開する。テストの無い起票や、RED を観測していないテストで済ませない。

対象アプリ側なら、アプリを直して再実行する。直しても同じ指紋の stuck がさらに 3 回続いたら、それ以上ループせず、切り分けの記録と出力を示してユーザーに AskUserQuestion で Ask する。

`waiting-human` は停滞ではない。`pages` と、`builtin:threejs-render` 種の能力は `sitecraft.toml` の trust、`command` 種の能力は argv（D2-trust）の trust が無いと走れない。どのファイル（`sitecraft.toml` か `sitecraft/decisions.toml` の `verify.argv`）に `sitecraft trust` が要るかを示して、人間に `sitecraft trust` を実行してもらうよう AskUserQuestion で Ask する。trust を待ち続けない・同じ検査を繰り返さない・LLM が代わりに `sitecraft trust` を実行しない。

### Phase 11 — 完成報告

`sitecraft check pages`・`sitecraft check security`・`sitecraft decide check`、および **relevant な能力すべての `sitecraft check capability <name>`** が**すべて exit 0** になったことを確認してから、プロトタイプ完成を報告する。報告には各コマンドの実行結果（exit code）を含める。1 つでも exit 1 / exit 2 が残っていれば完成と報告しない — 残っている検査とその出力をそのまま報告する。

`sitecraft capability probe` の `feasible` だけでは完成ではない。probe は動かせるかの事前確認であり、能力の完成は `check capability` の exit 0 でだけ示す。
