# backlog

> 🌐 [English](README.md) ・ **日本語**

Claude Code 向けの**クロスプロジェクト・タスクキュー** — どのセッション・どのリポジトリよりも長く生き残る、cycle-type タグ付きの作業項目の永続キュー。

## 目的

backlog は「あとでやる」をセッションをまたいで持ち越すための耐久キューである。責務は次の 2 つに集約される。

- **キューと state の管理**: `backlog` バイナリが作業項目の追加・一覧・ピック・完了/失敗マークを担い、複数セッションを直列化するための排他 run-lock (`~/.backlog/run.lock`) を所有する。
- **保留作業の自動浮上**: **SessionStart** フックが、セッションが開いた瞬間に pending なタスクを context として注入する。

cycle-type のタグでタスクを分類できるため、リポジトリ横断で「どの種類の仕事が溜まっているか」を後から絞り込める。lock→pick→`/condukt`→done のループ driver 自体は `/flow` に統合されており、同梱の `/backlog` skill はその薄いエイリアス兼 queue/state 操作のエントリポイントである。

**サブスクリプションネイティブ**: skill 1 つ、hook 1 つ、同梱の Rust バイナリ 1 つだけで動き、`ANTHROPIC_API_KEY` も追加インストールも不要。SessionStart フックは fail-soft で、壊れた stdin は stderr にログして読み飛ばし、常に exit 0 で返すのでターンを壊さない。

## どうして必要か

セッションは揮発する。会話を閉じれば「次にやろうと思っていたこと」も一緒に消え、別のリポジトリで作業を始めれば、別プロジェクトで積み残した課題は視界から完全に外れる。チャット履歴や記憶に頼っていると、保留タスクは静かに失われる。

backlog はこの失敗モードを潰す。一度キューに積めば、

- セッションを閉じても、別リポジトリに移っても、項目は永続キューに残り続ける。
- 次にどのプロジェクトでセッションを開いても、SessionStart フックが pending な作業を自動で context に差し込むので、「何が残っていたか」を思い出す必要がない。
- 排他 run-lock により、複数セッションが同時にキューを消化して競合することを防ぐ。`/flow` driver はキューを drain する前にロックを取得し、他セッションは `lock status` がアクティブな保有者を報告したら退避する。

つまり backlog が無いと、保留作業の追跡が人間（または揮発する会話）任せになり、取りこぼしと並行消化の衝突が起きる。

## どう使うか

プラグインマーケットプレイス経由で導入すると、同梱の `/backlog` skill がすぐ使える。`backlog` バイナリはキューと排他 run-lock を所有し、次のサブコマンドを公開する。

| サブコマンド | 役割 |
|---|---|
| `add` | タスクを追加 (`--title`, `--project`, `--tag`, `--priority p0/p1/p2`, `--notes`, `--weight`, `--force`) |
| `list` | store のタスク一覧。`--tag` / `--status` で絞り込み。repo store では `--project` は絞り込みではなく **assertion**（下の「スコープ」参照）|
| `next` | 次の最高優先度の pending タスクを JSON で出力 |
| `done <id>` | タスクを完了マーク |
| `fail <id>` | タスクを失敗マーク (`--reason`)。再実行を 2 日先送りする |
| `edit <id>` | タスクの title / tags / notes / status を更新 |
| `session-start` | SessionStart フック: pending タスクを context として注入 |
| `install` / `uninstall` | `~/.claude/settings.json` の SessionStart フックを配線/除去 |
| `lock {acquire,release,status}` | `~/.backlog/run.lock` 排他ロックの管理 |

### slash command

`/backlog` は queue・state 操作（`list` / `next` / `done` / `fail` / `lock`）を呼ぶ薄いエントリポイント。引数でサブコマンドを渡す。

> キューを自動で全件消化したいときは `/backlog` ではなく **`/flow`** を使う。lock 取得 → アイテムピック → `/condukt` → done/fail → lock 解放というループ driver は `/flow` に統合されており、compass ゲート・budgetguard・fugu-router によるモデル選択も含む上位互換 driver になっている。

### SessionStart フックの配線

プラグイン導入後、`backlog install` を実行すると `~/.claude/settings.json` に `SessionStart` グループがマージされる（冪等・所有権マーク付き）。これでセッションを開くたびに pending な作業が浮上する。`install` / `uninstall` は `--dry-run` で書き込まず結果だけ表示できる。

### 最小例（standalone / cargo）

```sh
cargo install --path .
backlog add --title "Fix X" --project "$PWD" --priority p1   # 項目をキューに積む
backlog list --status pending                                # キューを見る
backlog next                                                 # 次の項目をピック
backlog done <id>                                            # 解決する
backlog fail <id> --reason "blocked"                         # 2 日先送りする
backlog lock status                                          # run-lock の保有者を確認
backlog install                                              # SessionStart フックを settings.json にマージ
backlog uninstall                                            # 再び除去する
```

> 補足: `backlog list` の status 語彙は `pending` であり `open` ではない。`list --status open` は何も表示しない。

### 重複タスクの拒否 (content hashkey)

`add` はタイトルと project から求めた **content hashkey** (`title` を trim → Unicode NFKC → 小文字化 →
連続空白の1個への圧縮 → 前後の記号除去 したものと project を FNV-1a 64bit で畳み込んだ 16 桁 hex)
で内容の重複を検出する。次のいずれかに該当する場合、`add` はエラーで拒否される (`done` の重複はブロックしない
— 同じタイトルを再度積むことは正当なため):

- 同じ hashkey を持つ既存タスクが `pending` または `failed` である。
- `condukt` が PATH 上にあり、`condukt state is-claimed --hashkey <h>` が exit 0 (= 他セッションの
  live なクレームが握っている) を返す。`condukt` が不在、または上記以外の理由でエラー/非0終了した場合は
  fail-soft に倒し「クレームなし」として扱う (`condukt` の欠落や不調で `add` を失敗させない)。

どちらの拒否も `backlog add --force` で意図的にバイパスできる。

`backlog list --json` の各要素には `hashkey` フィールドが含まれる (title + project から計算、保存はされない)。
`/flow` など上位 driver がこれを使って `condukt state is-claimed` によるゲートを追加コストなしに行える。

### スコープ: repo ごとに 1 キュー、ファイル自体がスコープ

store は repo ごとに解決される (`<repo root>/.backlog/tasks.toml`。ほかのファイルと同じように
merge される tracked file)。したがってその中身はその repo のタスクだけであり、以下 2 点は意図的:

- **read は project フィルタを掛けない。** どの checkout が書いたものであっても、ファイル内の
  全タスクがスコープ内。書いた checkout の絶対パスで行を絞り込むと、1 つの repo のキューが
  **マシンごとに分裂**する（実測 2026-08-20、本 repo の store: pending 258 件が macOS の
  checkout パス、66 件が WSL のパスでラベルされ、どちらから `list` しても自分側しか見えなかった）。
  よって `--project` は「どの store のことを言っているか」の **assertion** になる — この repo を
  指すなら何も変わらず、別の repo を指すのは（絞り込み結果ではなく）エラー。`--all` は受理され、
  repo store ではそれが既定の挙動。
- **repo root が上に無い cwd には store が無い。** `add`/`list`/`next` は理由を述べて拒否し、
  cross-project な `~/.backlog` へフォールバックしない。そのフォールバックが、tempdir で走った
  プロセスが fixture を本物のキューへ書き込んだ経路であり、そういう cwd からの read が
  別プロジェクトの作業で答えられていた経路である。共有 store を明示的に使いたい場合は
  `~/.backlog/config.toml` で `store_dir` を pin する（pinned store は複数 project を持ちうるので、
  そこでは `--project` は従来どおりのフィルタ）。

`project` フィールド自体は残る（誰が起票したかを示し、そのラベルが推測だった場合は `list` が
`[project unresolved: …]` と表示する）が、**何が見えるかを決めるものではなくなった**。

### checkout 間の claim 排他 (`next --claim`)

store は意図的に checkout に追従する (`<repo root>/.backlog/tasks.toml`。linked worktree は
それ自体が root。CLAUDE.md §8 が worktree から main のトラックファイルを書くことを禁じるため)。
したがって同一プロジェクトの 2 つの checkout は乖離した 2 つのファイルを持つ。claim の排他は
store の隣に置く lockfile = checkout 単位だったので、両者が **同じタスク** を配ってしまっていた。

`next --claim` は今、より **広い** ロックを先に取り、claim を store の場所ではなく project の
**identity** で鍵付けした machine-global な ledger に記録する:

- ledger: `~/.backlog/claims/<project-slug>.json` (`<project-slug>` は `backlog lock` と同じ
  FNV-1a の project ハッシュ。linked worktree は main working tree に正規化されるので、同一
  プロジェクトの全 checkout が 1 つの ledger を共有する)
- ロック順序 (逆順にしないこと): `~/.backlog/claims/<slug>.lock` (project 全体) → `<store>.lock` (この checkout)
- entry は 1h (`CLAIM_STALE_SECS`) で除外をやめる。store 側の stale-claim 再取得と同じ窓なので、
  死んだ claimant が全 checkout でタスクを永久にロックすることはない。記録自体は 7 日保持する。

このパス上の判定不能はすべて **claim を拒否** し、理由を stderr に出して非0終了する。exit 0 +
`no pending tasks` (= driver は「仕事がない」と読む) には決して倒さない。対象は: ledger ディレクトリを
作れない / ledger ロックを取れない / ledger が読めない・パースできない・書けない / tasks-file ロックを
保持できない / project identity を解決できない。**拒否は空のキューではない。**

同梱の `bin/backlog-*` バイナリがプラグインの出荷物なので、エンドユーザーは cargo も API キーも不要。skill や hook が依存する挙動を変えたら、ワークスペースをビルド（`cargo build --workspace --release`）して再コミットする。
