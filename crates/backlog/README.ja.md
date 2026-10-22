**SessionStart** フックは、セッションが開いた瞬間に pending なタスクを context として注入する。

キューを自動で全件消化したいときは `/flow` を使うこと。

## サブコマンドを使うこと

- `add` — `--repro-test` のコミット済みテストが HEAD で落ちた時だけタスクを追加する
- `list` — store のタスク一覧: `--tag` / `--status`（`pending` / `done` / `failed` / `cancelled` / `claimed`、`claimed` は導出値、`open` は status ではない）/ `--json`。repo store では `--project` は絞り込みではなく **assertion**（下の「スコープ」参照）
- `next` — reproduced な repro を持つ pending タスクのうち最高優先度のものを JSON で出力する（`--claim` で予約）
- `done <id>` — タスクを**証拠付きで**完了マークする: `--test CMD --red-rev REV`（`--reason fixed|already-fixed|obsolete`）・`--doc-only COMMIT`・`--duplicate-of ID`。この 3 形式のちょうど 1 つが必要。素の `done <id>` は拒否される
  - RED の実行は `--red-rev` の detached な一時 worktree に HEAD のテストファイルを上書きしたもの（`cargo test` は全テストファイル、pytest は名指したファイル、`bash`/`sh` はそのスクリプト）で行う。修正と一緒に足したテストがそこに存在するようにするため。`--red-rev` でテストが通ったことによる拒否はその旨を述べる。修正そのものがテストファイルの中にあると、この上書きが修正を RED 側へ持ち込む。`--red-overlay PATH`（複数可）で上書きを名指した repro 側だけに絞れる。絞ることしかできず、各パスは既定の上書き集合に含まれている必要がある。`--red-rev` で通るテストは依然拒否される。絞った集合は `closure.red.overlay` に記録される。
- `fail <id>` — タスクを失敗マークする (`--reason`)。再実行を 2 日先送りする
- `cancel <id> --reason R` — 「やらないと決めた」としてテスト無しで閉じ、`discard` closure として記録する
- `audit-closures` — 全終端行の closure 証拠を read-only で分類する（`--json`）。store は書かない
- `overlap` — キュー中のタスクを宣言された `touched_files` scope でグループ化する（advisory、未宣言は別に列挙）
- `edit <id>` — タスクの title / tags / notes / status を更新する
- `sync` — store を GitHub issue と突き合わせる（一方向・ローカル優先）。`--apply` なしでは報告のみ。`--only both|create|close|body` で実行する arm を選ぶ。`--limit` は plan をそのタスク数に切り詰める
- `session-start` — SessionStart フック: pending タスクを context として注入する
- `install` / `uninstall` — `~/.claude/settings.json` の SessionStart フックを配線/除去する（standalone 用）
- `merge-driver` — task store 用の git merge driver（下記参照）
- `lock {acquire,release,status,heartbeat,probe}` — project ごとの排他ロック `~/.backlog/locks/<project-slug>.lock` を管理する。`probe` は保有者の進捗（git HEAD + transcript の伸び）を判定する。`stalled` の保有者だけが `acquire` で reap されうる。保有中（active でも heartbeat が stale でも）は保有セッション以外の `next --claim` が非 0 で拒否される。`CLAUDE_CODE_SESSION_ID` を持たない claim と、ロックファイルが読めないときの claim も拒否される
- `driver {register,heartbeat,unregister,status}` — project のキュー driver を排他せずに通知・観測する。`heartbeat` は自セッションの `next --claim` lease も更新する


### store 書き込みは linked worktree で

`add` / `done` / `fail` / `edit` / `cancel` / `sync --apply` は、store が git リポジトリの **primary（main）作業ツリー**内にある場合
**拒否**される（非 0 終了・何も書かない）。
linked worktree から実行すること。
git が checkout の種別を答えられない場合も `add` を拒否する。
`list` は primary でも動く。
`next --claim` も primary で許可され、`git status` は clean のまま。
SessionStart フックは primary では requeue をスキップし、その旨を表示する。

プラグインの `hooks/hooks.json` が SessionStart フック（`backlog session-start`）を登録する。登録されたフックの timeout は 10 秒。`backlog install` は standalone（cargo）利用向けで、`SessionStart` グループを `~/.claude/settings.json` にマージする（冪等・所有権マーク付き）。`install` / `uninstall` は `--dry-run` で書き込まず結果だけ表示できる。

### 最小例を standalone（cargo）で実行すること

```sh
cargo install --path .
backlog add --title "Fix X" --project "$PWD" --priority p1 \
  --repro-test "bash tests/repro_fix_x.sh"                  # コミット済みテストが HEAD で落ちた時だけ起票
backlog list --status pending
backlog next                                                 # 次の項目をピック
backlog done <id> --test "cargo test -p x" --red-rev <rev>  # 解決する（REV で RED・HEAD で GREEN を実測）
backlog done <id> --doc-only <commit>                        # または doc だけの祖先 commit
backlog done <id> --duplicate-of <id>                        # または重複
backlog fail <id> --reason "blocked"                         # 2 日先送りする（2 日後に pending へ戻る）
backlog cancel <id> --reason "やらないと決めた"               # やらないと決めて捨てる（終端、再キューしない、`discard` closure として記録、テスト不要）
backlog lock status                                          # project の排他ロックの保有者を確認
backlog install                                              # SessionStart フックを settings.json にマージ
backlog uninstall                                            # 再び除去する
```

> 補足: `backlog list` の status 語彙は `pending` であり `open` ではない。

### テストが無ければ起票しない

規則と根拠テストは [backlog スキルの「起票の基準」](skills/backlog/SKILL.md) を参照すること。

`add` はタイトルと project から求めた **content hashkey** (`title` を trim → Unicode NFKC → 小文字化 →
連続空白の1個への圧縮 → 前後の記号除去 したものと project を FNV-1a 64bit で畳み込んだ 16 桁 hex)
で内容の重複を検出する。次のいずれかに該当する場合、`add` はエラーで拒否される。`done` の重複はブロックしない:

- 同じ hashkey を持つ既存タスクが `pending` または `failed` である。
- `condukt` が（plugin cache を先に、次に PATH を探して）見つかり、`condukt state is-claimed --hashkey <h>` が exit 0 (= 他セッションの
  live なクレームが握っている) を返す。exit 1 は stdout に `"claimed": false` が付いている場合に限り
  「クレームなし」と読む。それ以外 — `condukt` が見つからない・起動失敗、0/1 以外の exit code (例: 3 = claim registry
  を読めない)、`claimed` フィールドを伴わない exit 0/1、exit 1 に `"claimed": true` (exit と
  claimed の不一致)、シグナル終了、時間内に答えが返らない — は
  クレーム確認ができなかったことを意味し、`add` は理由を添えて**拒否**される (live な claim 済みタスクの
  重複を排除できないため)。「判定不能」を「クレームなし」とは読まない。

いずれの拒否も `backlog add --force` で意図的にバイパスできる。

`backlog list --json` の各要素には `hashkey` フィールドが含まれる (title + project から計算、保存はされない)。

store は repo ごとに解決される (`<repo root>/.backlog/tasks.toml`)。

- **read は project フィルタを掛けない:** どの checkout が書いたものであっても、ファイル内の
  全タスクがスコープ内。

  - 本 repo の store で pending/failed をラベル別に数えると macOS 265、WSL 70、`C:/…` 5。

  再測定コマンドを実行すること。

  ```sh
  python3 -c "import collections,tomllib; d=tomllib.load(open('.backlog/tasks.toml','rb')); \
    print(collections.Counter(t['project'] for t in d['task'] if t['status'] in ('pending','failed')).most_common())"
  ```

  よって `--project` は「どの store のことを言っているか」の **assertion** になる — この repo を
  指すなら何も変わらず、別の repo を指すのは（絞り込み結果ではなく）エラー。`--all` は受理される。
- **repo root が上に無い cwd には store が無い:** `add` と `list` は理由を述べて拒否し、
  cross-project な `~/.backlog` へフォールバックしない。`next` も同様に拒否する。共有 store を明示的に使いたい場合は
  `~/.backlog/config.toml` で `store_dir` を pin する（pinned store は複数 project を持ちうるので、
  そこでは `--project` は従来どおりのフィルタ）。

`project` フィールド自体は残る（誰が起票したかを示し、そのラベルが推測だった場合は `list` が
`[project unresolved: …]` と表示する） が、**何が見えるかを決めるものではなくなった**。

store は兄弟関係の 2 ファイルからなる: `tasks.toml`（live キュー）と `tasks.done.toml`
（終端状態 — `done` / `cancelled` — の行すべて）。`.backlog/tasks.toml` の done ファイルは
`.backlog/tasks.done.toml`。

- **読み取りは和集合を見る:** reader は両ファイルの行を返す。
  `list --status done`・`done <id>`・`edit <id>` は、done ファイルにしか無い行も見る。
  `sync` も同様に見る。near-duplicate 走査は終端行を peer として出さない。重複ガードの判定は従来どおり（`done` 行はどちらのファイルに
  あっても再 add をブロックしない）。
- **終端が勝ち、終端は最終:** 同じ id が両ファイルにあるとき（例: git merge が完了済みタスクを
  `tasks.toml` に `pending` として戻した）、返るのは終端側の行 1 件で、`next` はそれを渡さない。
  `done`/`cancelled` のタスクへの `edit --status pending|failed` は非0終了で拒否され、`fail` も
  拒否される。タスクは終端のまま。既に done のタスクへの `done` は従来どおり冪等な成功。
- **壊れた done ファイルはエラーであり、空ではない:** `tasks.done.toml` が無いのは、まだ何も
  完了していないというだけ。存在するのに読めない/パースできない場合、`list` と `next` はそのファイル名を含むエラーで非0終了し、done 行を黙って欠いた一覧は出さない。`add`・`done`・`edit`・`fail`・`sync` も同様に非0終了し、done 行を上書きする書き込みは起こさない。SessionStart フックは空キューでは
  なく「store UNREADABLE」通知を注入する。
- **書き込みは振り分ける:** 保存のたびに終端行は done ファイルへ、それ以外は `tasks.toml` へ書く。新たに終端になった行は末尾に追記されるので、完了の git diff は純粋な追記に
  なる。done ファイルに既にある行は位置を保ち、内容が変わったとき（`sync` の記録や notes 編集）だけ
  その場で書き換える。done ファイルを先に書き、中身が変わらなければ書き直さない。`tasks.toml` はアトミックに置換
  される（fsync した一時ファイル + rename）。2 回の書き込みの間でクラッシュすると行が両ファイルに
  残るが、終端優先の和集合で解決される。
- **古いバイナリの行は次の書き込みで移行される:** `tasks.toml` に残った
  `done`/`cancelled` 行は通常どおり一覧に出て、いずれかのコマンドが store を保存した
  次の機会に done ファイルへ移る。

`.gitattributes` は `.backlog/tasks.toml` と `.backlog/tasks.done.toml` の両方に `merge=backlog` を指定する。
`backlog merge-driver <base> <ours> <theirs>`（git の
`%O %A %B`）が行単位ではなくタスク id 単位でマージする:

- id の和集合をとる。片側で（base に対して）削除され他方で未変更のタスクは削除のまま。片側だけで
  変更されたタスクはその側を採る。
- 同じ id が両側で異なって編集された場合は
  **conflict**: どちらも採らず非 0 終了する。edit/delete、同じ新 id が異なる内容で追加された場合も同様。`<ours>` は手を付けず、git がファイルを unmerged として
  人間に返す。
- パースできない入力は非 0 終了で `<ours>` は不変。
- 文字列 `id` を持たない行、1 入力内の id 重複、未知の
  top-level キーの場合も非 0 終了で `<ours>` は不変。
  clean な結果はアトミックに書かれる。

repo ごとに driver を設定するには次を実行すること。

```sh
backlog merge-driver --install   # repo 内で実行すること
```

これは repo の `merge.backlog.driver` を `backlog merge-driver %O %A %B` を実行するよう設定する。

store は意図的に checkout に追従する (`<repo root>/.backlog/tasks.toml`、linked worktree は
それ自体が root)。

`next --claim` は、より **広い** ロックを先に取り、claim を store の場所ではなく project の
**identity** で鍵付けした machine-global な ledger に記録する:

- ロック順序 (逆順にしないこと): `~/.backlog/claims/<slug>.lock` (project 全体) → `<store>.lock` (この checkout)
- ledger: `~/.backlog/claims/<project-slug>.json`、linked worktree は main working tree に正規化されるので、同一
  プロジェクトの全 checkout が 1 つの ledger を共有する。`<project-slug>` は `backlog lock` と同じ
  FNV-1a の project ハッシュ。
- entry (**lease**) は holder の生存が 1h (`CLAIM_STALE_SECS`) 観測されないと除外をやめるので、
  死んだ claimant が全 checkout でタスクを永久にロックすることはない。記録自体は 7 日保持する。
- 「生存の観測」= claim 時刻と holder の最後の lease heartbeat のうち新しい方。`next --claim` は
  claim したセッション (`CLAUDE_CODE_SESSION_ID`) を記録し、`backlog driver heartbeat --session-id S
  --project P` は P の ledger 上の S の **まだ生きている** lease も更新する。したがって 1h 以内ごとに
  heartbeat する driver は、タスクに何時間かかっても claim を保つ (backlog 486e83ca)。期限切れの
  lease は蘇生しない。セッションの無い lease (旧バイナリの entry やセッション外の claim) は従来どおり
  claim 時刻だけで期限切れになる。lease を更新できない (例: ledger が読めない) 場合、heartbeat はその旨を出して非0終了する。heartbeat フィールドが壊れていれば
  ledger 全体がパース不能となり、holder を死んだとはみなさず claim を拒否する。

このパス上の判定不能はすべて **claim を拒否** し、理由を stderr に出して非0終了する。exit 0 +
`no pending tasks` (= driver は「仕事がない」と読む) には決して倒さない。対象は: ledger ロックを取れない / ledger が読めない・パースできない / tasks-file ロックを
保持できない / project identity を解決できない。拒否は空のキューではない。ledger ディレクトリを作れない場合と ledger が書けない場合も同様に拒否される。

#### lease はトラックされた store に書かない — `claimed` は導出値

claim が書くのは **ledger だけ** である。`next --claim` はトラックされた `.backlog/tasks.toml` を
変更しない。SessionStart の requeue も claim を
理由に行を書き換えない。したがって claim しても git worktree は汚れない。`claimed` は **導出**
ステータスで、行ごとに保存されたステータスと生きた lease (あれば) から決まる:

- 生きた lease を持つ `pending` 行は `claimed` と表示する。
- 生きた lease を持つ `failed` 行は、lease がその行の `updated_at` より **厳密に後** に取られた
  場合 (古い failed タスクの再 claim) だけ `claimed` と表示する。claim と同時かそれ以降に更新された
  行 (claimant が `fail` した) は `claimed` と表示せず、`--status failed` がそれを選ぶ。
- `done`/`cancelled` 行は決して `claimed` と表示しない。

この規則は表示だけのものである。`next` / `next --claim` からの除外は生きた lease だけで決まるので、
claimant が直前に fail したタスクは `claimed` と表示されないが、lease が期限切れになるまで配られない。

- `next --claim` は従来どおり `"status": "claimed"` でタスクを出力する。
- 素の `next` は lease 中のタスクを返さない。`list` (テキストと `--json`) は上記の導出ステータスを表示する。
  status フィルタは導出後に適用される: `--status claimed` は
  `claimed` と表示される行を選ぶ。`--status pending` は lease 中を含まない。
- 旧バイナリが `status = "claimed"` で保存した行は `pending` として読む。除外するのは生きた lease
  だけ。その行は、無関係なコマンドが次に store を保存したときに `pending` として書き直される。
- `done`/`fail` は lease を解放しない。`edit --status pending` も lease を解放しない。終端でないタスクの lease は、holder の
  最後の生存観測から 1h で期限切れになるまで除外を続ける。
- `list`・素の `next`・`next --claim` は同じ方法 (上記の project identity) で ledger を特定する。
  repo 外ではその identity は正規化した cwd。identity を解決できない (例: worktree の `.git` リンクが
  切れている) か、ledger が存在するのに読めない・パースできない場合、`list` と素の `next` は
  **拒否** する (非0終了、理由は stderr、stdout は空)。claim 済みかもしれないタスクを pending と
  表示することはない。
- store 乖離チェックは lease 中のタスクを「キューに残る作業」に数えない。

git で追跡しているのは `bin/backlog` ランチャーだけである。ランチャーは配布先プラグインで隣に置かれたプラットフォーム別の `backlog-<os>-<arch>` バイナリを exec し、ホスト用のビルドが無ければ（空出力で exit 0 ではなく）非 0 で終了する。
