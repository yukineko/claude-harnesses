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
  **マシンごとに分裂**する。本 repo の store で pending/failed をラベル別に数えると:

  | 測定点 | macOS ラベル | WSL ラベル | `C:/…` ラベル |
  |---|---|---|---|
  | `bb046648` (2026-08-20) | 258 | 66 | 5 |
  | `89feaddb` (2026-08-20) | 265 | 70 | 5 |

  WSL の checkout からの `list` は WSL ラベルだけを拾い、残りを黙って落としていた
  （どれも、いま読んでいるまさにその repo のタスク）。再測定コマンド:

  ```sh
  python3 -c "import collections,tomllib; d=tomllib.load(open('.backlog/tasks.toml','rb')); \
    print(collections.Counter(t['project'] for t in d['task'] if t['status'] in ('pending','failed')).most_common())"
  ```

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

### 2 ファイル: live キューと done ファイル

store は兄弟関係の 2 ファイルからなる: `tasks.toml`（live キュー）と `tasks.done.toml`
（終端状態 — `done` / `cancelled` — の行すべて）。一般に `<dir>/<stem>.toml` の done ファイルは
`<dir>/<stem>.done.toml`。

- **読み取りは和集合を見る。** すべての reader は両ファイルの行を返す単一の loader を通る。
  `list --status done`・`done <id>`・`edit <id>`・`sync`・重複ガード・near-duplicate 走査は、
  done ファイルにしか無い行も見る。重複ガードの判定は従来どおり（`done` 行はどちらのファイルに
  あっても再 add をブロックしない）。
- **終端が勝ち、終端は最終。** 同じ id が両ファイルにあるとき（例: git merge が完了済みタスクを
  `tasks.toml` に `pending` として戻した）、返るのは終端側の行 1 件で、`next` はそれを渡さない。
  `done`/`cancelled` のタスクへの `edit --status pending|failed` は非0終了で拒否され、`fail` も
  拒否される。タスクは終端のまま（既に done のタスクへの `done` は従来どおり冪等な成功）。
- **壊れた done ファイルはエラーであり、空ではない。** `tasks.done.toml` が無いのは、まだ何も
  完了していないというだけ。存在するのに読めない/パースできない場合、store を読むコマンド
  (`list`・`next`・`add`・`done`・`edit`・`fail`・`sync`) はそのファイル名を含むエラーで非0終了する —
  done 行を黙って欠いた一覧も、それを上書きする書き込みも起こさない。SessionStart フックは空キューでは
  なく「store UNREADABLE」通知を注入する。
- **書き込みは振り分ける。** 保存のたびに終端行は done ファイルへ、それ以外は `tasks.toml` へ書く。
  done ファイルに既にある行は位置を保ち（内容が変わったとき — `sync` の記録や notes 編集 — だけ
  その場で書き換える）、新たに終端になった行は末尾に追記されるので、完了の git diff は純粋な追記に
  なる。done ファイルを先に書き、中身が変わらなければ書き直さない。各ファイルはアトミックに置換
  される（fsync した一時ファイル + rename）。2 回の書き込みの間でクラッシュすると行が両ファイルに
  残るが、終端優先の和集合で解決される。
- **古いバイナリの行は次の書き込みで移行される。** 0.3.7 より古い backlog が `tasks.toml` に残した
  `done`/`cancelled` 行は通常どおり一覧に出て、いずれかのコマンドが lock 下で store を保存した
  次の機会に done ファイルへ移る。

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
- entry (**lease**) は 1h (`CLAIM_STALE_SECS`) で除外をやめるので、死んだ claimant が全 checkout
  でタスクを永久にロックすることはない。記録自体は 7 日保持する。

このパス上の判定不能はすべて **claim を拒否** し、理由を stderr に出して非0終了する。exit 0 +
`no pending tasks` (= driver は「仕事がない」と読む) には決して倒さない。対象は: ledger ディレクトリを
作れない / ledger ロックを取れない / ledger が読めない・パースできない・書けない / tasks-file ロックを
保持できない / project identity を解決できない。**拒否は空のキューではない。**

#### lease はトラックされた store に書かない — `claimed` は導出値

claim が書くのは **ledger だけ** である。`next --claim` はトラックされた `.backlog/tasks.toml` を
変更しない (一貫した読み取りのために tasks-file ロックは今も取る)。SessionStart の requeue も claim を
理由に行を書き換えない。したがって claim しても git worktree は汚れない。`claimed` は **導出**
ステータスで、行ごとに保存されたステータスと生きた lease (あれば) から決まる:

- 生きた lease を持つ `pending` 行は `claimed` と表示する。
- 生きた lease を持つ `failed` 行は、lease がその行の `updated_at` より **厳密に後** に取られた
  場合 (古い failed タスクの再 claim) だけ `claimed` と表示する。claim と同時かそれ以降に更新された
  行 (claimant が `fail` した) は `failed` と表示し、`--status failed` がそれを選ぶ。
- `done`/`cancelled` 行は決して `claimed` と表示しない。

この規則は表示だけのものである。`next` / `next --claim` からの除外は生きた lease だけで決まるので、
claimant が直前に fail したタスクは `failed` と表示されつつ、lease が期限切れになるまで配られない。

- `next --claim` は従来どおり `"status": "claimed"` でタスクを出力する。
- 素の `next` は lease 中のタスクを返さない。`list` (テキストと `--json`) は上記の導出ステータスを表示する。
  status フィルタは導出後に適用される: `--status pending` は lease 中を含まず、`--status claimed` は
  `claimed` と表示される行を選ぶ (`claimed` は保存されるステータスではないので "unknown status" 警告は出る)。
- 旧バイナリが `status = "claimed"` で保存した行は `pending` として読む。除外するのは生きた lease
  だけ。その行は、無関係なコマンドが次に store を保存したときに `pending` として書き直される。
- `done`/`fail`/`edit --status pending` は lease を解放しない。終端でないタスクの lease は 1h で
  期限切れになるまで除外を続ける。
- `list`・素の `next`・`next --claim` は同じ方法 (上記の project identity) で ledger を特定する。
  repo 外ではその identity は正規化した cwd。identity を解決できない (例: worktree の `.git` リンクが
  切れている) か、ledger が存在するのに読めない・パースできない場合、`list` と素の `next` は
  **拒否** する (非0終了、理由は stderr、stdout は空)。claim 済みかもしれないタスクを pending と
  表示することはない。
- store 乖離チェックは lease 中のタスクを「キューに残る作業」に数えない (以前、保存された
  `claimed` 行を数えなかったのと同じ)。

同梱の `bin/backlog-*` バイナリがプラグインの出荷物なので、エンドユーザーは cargo も API キーも不要。skill や hook が依存する挙動を変えたら、ワークスペースをビルド（`cargo build --workspace --release`）して再コミットする。
