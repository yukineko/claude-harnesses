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

1. **未回答のまま scaffold へ進まない。** `sitecraft decide next` が `{"status":"done"}` を返し、かつ `sitecraft decide check` が exit 0 になるまで、scaffold・実装に入らない。
2. **仮の既定値で埋めない。** ユーザーが答えていない問いに LLM が「とりあえず」「一般的には」で答えを入れて `decide record` してはならない。わからない問いは AskUserQuestion で聞く。「関係なし（irrelevant）」も**ユーザーの判断を経た記録**であって、問いを省略する手段ではない。
3. **テスト作者と実装者を別 subagent に分ける。** 自分の実装を検証するテストを自分で書かない（CLAUDE.md §2(a)）。
4. **E2E は既存の `webqa-gen` スキルに委ねる。** Playwright のテスト生成をこのスキル内で再実装しない。
5. **完成報告は全検査 exit 0 の後だけ。** `sitecraft check pages`・`sitecraft check security`・`sitecraft decide check` がすべて exit 0 になってからプロトタイプ完成を報告する。exit 2 は合格扱いしない。

## 手順

### Phase 1 — 目的を聞く

1. 作りたいもの（目的）を **AskUserQuestion** で聞く。引数で渡されていても、誰が何のために使うか・最低限動いてほしいことを確認する。
2. 目的を 1〜3 文に要約してユーザーに確認する。以降の判定はこの要約を根拠にする。

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
sitecraft decide add-question --id <id> --text "<問い>" [--choice <選択肢> ...]
```

`--choice` を付けない問いは自由記述。

### Phase 4 — 全問に答えてもらう（Ask ループ）

次のループを `decide next` が done を返すまで回す。

1. `sitecraft decide next` を実行する。出力は JSON:
   - `{"status":"ask","area":…,"question_id":…,"text":…,"choices":[…]}` — 次の未回答の問い
   - `{"status":"done"}` — すべて回答済み
   - exit 2 — 記録が壊れている・読めない。メッセージの fix に従って直す（握りつぶして先へ進まない）
2. `ask` なら、その `text` と `choices` を **AskUserQuestion** でユーザーに聞く（`choices` が空なら自由記述）。
3. 回答を記録する:
   ```sh
   sitecraft decide record --area <area> --question <question_id> --answer <回答>
   ```
   ドメイン問い（Phase 3）は `--area domain --question <id>` で記録する。
4. 1. に戻る。

done が返ったら `sitecraft decide check` を実行し、**exit 0 を確認する**。exit 1 なら列挙された未回答を Ask ループで埋める。exit 2 なら記録の不整合を直す。exit 0 になるまで Phase 5 以降へ進まない。

### Phase 5 — 計画と切り出し（simple / dedicated-harness）

記録した判断から実装する機能を列挙し、機能ごとに分類して記録する。

```sh
# このサイトの中で素直に実装できるもの
sitecraft decide classify --feature <name> --kind simple
# 独立した検査・ゲート・状態管理が要るもの（専用 harness 化）
SITECRAFT_MARKETPLACE=<harness repo>/.claude-plugin/marketplace.json \
  sitecraft decide classify --feature <name> --kind dedicated-harness [--harness <plugin>]
```

`dedicated-harness` の出力 JSON の `existing` を見る:

- `existing: true` — 既存の専用 plugin がある。その plugin を使う（README を読んで導入し、再実装しない）。
- `existing: false` — 無い。harness repo に新規 plugin を作る。雛形（`crates/<plugin>/`）:
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

### Phase 6 — scaffold（既定 Vite）

- 既定は **Vite**（`npm create vite@latest <dir>`）。Next.js 等は**ユーザーが選んだ場合だけ**使う（選択制。AskUserQuestion で確認する）。
- **空でないディレクトリには scaffold しない。** 対象ディレクトリにファイルがあれば拒否し、別のディレクトリをユーザーに聞く（上書き・マージはしない）。

### Phase 7 — sitecraft.toml と trust

プロジェクト直下に `sitecraft.toml` を書く（キーはすべてトップレベル）:

```toml
command = ["npm", "run", "dev", "--", "--port", "5173", "--strictPort"]
port = 5173
ready_path = "/"
ready_timeout_secs = 60
pages = ["/"]
build_dir = "dist"
web_globs = ["src/**", "index.html", "public/**"]
```

書いたら **`sitecraft trust` は人間に実行してもらう。** `command` は Stop gate と `sitecraft serve start` が実行するコマンドなので、内容を人間が確認して承認する必要がある。LLM が代わりに `sitecraft trust` を実行しない。未 trust の設定では `serve start` も gate も止まる（exit 2 / block）。

### Phase 8 — 実装

- 機能ごとに、**テスト作者 subagent** と **実装者 subagent** を分ける。テスト作者が先に RED を観測したテストを書き、実装者はそのテストを変更せずに GREEN にする。
- 開発中の動作確認は `sitecraft serve start` / `sitecraft serve status` / `sitecraft serve stop`。

### Phase 9 — E2E（webqa-gen に委ねる）

E2E テストは既存の **`/webqa-gen`** スキルで生成する（Playwright の spec 生成・RED 観測・弱体化検査はそちらの責務。ここで再実装しない）。webqa-gen も実装者とは別の subagent で回す。

### Phase 10 — 検査

ビルドしてから、次をすべて実行する:

```sh
npm run build
sitecraft serve start
sitecraft check pages
sitecraft serve stop
sitecraft check security
sitecraft decide check
```

- `check pages` — 設定した pages を Playwright で開き、console error・page error・asset の HTTP エラー・壊れたリンクを検出する。
- `check security` — build 出力の秘密情報（伏字で表示）・`.env` の値の混入・危険な HTML sink・`npm audit` の high/critical を検出する。
- exit 1 は指摘を直して再実行。exit 2 は検査が走れなかった原因（未ビルド・Playwright 未導入・lockfile 無し等）を直して再実行。

### Phase 11 — 完成報告

`sitecraft check pages`・`sitecraft check security`・`sitecraft decide check` が**すべて exit 0** になったことを確認してから、プロトタイプ完成を報告する。報告には各コマンドの実行結果（exit code）を含める。1 つでも exit 1 / exit 2 が残っていれば完成と報告しない — 残っている検査とその出力をそのまま報告する。
