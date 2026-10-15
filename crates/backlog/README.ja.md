# backlog

> 🌐 [English](README.md) ・ **日本語**

Claude Code 向けの**クロスプロジェクト・タスクキュー** — どのセッション・どのリポジトリよりも長く生き残る、cycle-type タグ付きの作業項目の永続キュー。

## 目的

backlog は「あとでやる」をセッションをまたいで持ち越すための耐久キューである。責務は次の 2 つに集約される。

- **キューと state の管理**: `backlog` バイナリが作業項目の追加・一覧・ピック・完了/失敗マークを担い、task 単位の予約（`next --claim`）と、明示的に全セッションを締め出すための project ごとの排他ロック (`~/.backlog/locks/<project>.lock`) を所有する。
- **保留作業の自動浮上**: **SessionStart** フックが、セッションが開いた瞬間に pending なタスクを context として注入する。

cycle-type のタグでタスクを分類できるため、リポジトリ横断で「どの種類の仕事が溜まっているか」を後から絞り込める。driver 登録→`next --claim`→`/condukt`→done のループ driver 自体は `/flow` に統合されており、同梱の `/backlog` skill はその薄いエイリアス兼 queue/state 操作のエントリポイントである。

**サブスクリプションネイティブ**: skill 1 つ、hook 1 つ、同梱の Rust バイナリ 1 つだけで動き、`ANTHROPIC_API_KEY` も追加インストールも不要。SessionStart フックは fail-soft で、壊れた stdin は stderr にログして読み飛ばし、常に exit 0 で返すのでターンを壊さない。

## どうして必要か

セッションは揮発する。会話を閉じれば「次にやろうと思っていたこと」も一緒に消え、別のリポジトリで作業を始めれば、別プロジェクトで積み残した課題は視界から完全に外れる。チャット履歴や記憶に頼っていると、保留タスクは静かに失われる。

backlog はこの失敗モードを潰す。一度キューに積めば、

- セッションを閉じても、別リポジトリに移っても、項目は永続キューに残り続ける。
- 次にどのプロジェクトでセッションを開いても、SessionStart フックが pending な作業を自動で context に差し込むので、「何が残っていたか」を思い出す必要がない。
- `next --claim` が選択と予約を同一クリティカルセクションで行うので、複数セッションが同時にキューを消化しても同じタスクを渡されない。`/flow` はキュー全体をロックせず（同じ project を複数セッションが回せる）、`driver` 登録と task 単位の `next --claim` を使う。排他ロック（`lock acquire`）は明示的に全セッションを締め出したいときだけ使い、他セッションは `lock status` がアクティブな保有者を報告したら退避する。

つまり backlog が無いと、保留作業の追跡が人間（または揮発する会話）任せになり、取りこぼしと並行消化の衝突が起きる。

## どう使うか

プラグインマーケットプレイス経由で導入すると、同梱の `/backlog` skill がすぐ使える。`backlog` バイナリはキューと排他 run-lock を所有し、次のサブコマンドを公開する。

| サブコマンド | 役割 |
|---|---|
| `add` | `--repro-test` のコミット済みテストが HEAD で落ちた時だけタスクを追加する [test: crates/backlog/tests/repro_evidence.rs::reproduced_lands_pending_and_records_outcome] |
| `list` | store のタスク一覧。`--tag` / `--status`（`pending` / `done` / `failed` / `cancelled` / `claimed`。`claimed` は導出値）/ `--json`。repo store では `--project` は絞り込みではなく **assertion**（下の「スコープ」参照）|
| `next` | reproduced な repro を持つ pending タスクのうち最高優先度のものを JSON で出力する（`--claim` で予約） [test: crates/backlog/tests/no_confirm_no_unproven.rs::next_hands_out_the_reproduced_row_over_a_repro_less_one] [test: crates/backlog/src/store.rs::next_returns_highest_priority] |
| `done <id>` | タスクを**証拠付きで**完了マーク。`--test CMD --red-rev REV`（`--reason fixed\|already-fixed\|obsolete`）・`--doc-only COMMIT`・`--duplicate-of ID` のちょうど 1 つが必要。素の `done <id>` は拒否される |
| `fail <id>` | タスクを失敗マーク (`--reason`)。再実行を 2 日先送りする |
| `cancel <id> --reason R` | 「やらないと決めた」としてテスト無しで閉じ、`discard` closure として記録する [test: crates/backlog/tests/cancel_evidence.rs::cancel_pending_task_records_a_discard_closure_without_tty] |
| `audit-closures` | 全終端行の closure 証拠を read-only で分類（`--json`）。store は書かない |
| `overlap` | キュー中のタスクを宣言された `touched_files` scope でグループ化（advisory。未宣言は別に列挙） |
| `edit <id>` | タスクの title / tags / notes / status を更新 |
| `sync` | store を GitHub issue と突き合わせる（一方向・ローカル優先）。`--apply` なしでは報告のみ。`--only both\|create\|close\|body`、`--limit` |
| `session-start` | SessionStart フック: pending タスクを context として注入 |
| `install` / `uninstall` | `~/.claude/settings.json` の SessionStart フックを配線/除去（standalone 用） |
| `merge-driver` | task store 用の git merge driver（下記参照） |
| `lock {acquire,release,status,heartbeat,probe}` | project ごとの排他ロック `~/.backlog/locks/<project>.lock` の管理。`probe` は保有者の進捗（git HEAD + transcript の伸び）を判定し、`stalled` の保有者だけが `acquire` で reap されうる |
| `driver {register,heartbeat,unregister,status}` | project のキュー driver を排他せずに通知・観測する。`heartbeat` は自セッションの `next --claim` lease も更新する |

### store 書き込みは linked worktree で

`add` / `done` / `fail` / `edit` / `cancel` / `sync --apply` は、store が git リポジトリの **primary（main）作業ツリー**内にある場合
**拒否**される（非 0 終了・何も書かない） [test: crates/backlog/tests/store_worktree_boundary.rs::add_from_primary_checkout_is_refused_and_writes_nothing] [test: crates/backlog/tests/store_worktree_boundary.rs::done_from_primary_checkout_is_refused_and_file_unchanged] [test: crates/backlog/tests/store_worktree_boundary_env.rs::fail_from_primary_checkout_is_refused_and_file_unchanged] [test: crates/backlog/tests/store_worktree_boundary.rs::edit_from_primary_checkout_is_refused_and_file_unchanged] [test: crates/backlog/tests/store_worktree_boundary_env.rs::cancel_from_primary_checkout_is_refused_and_file_unchanged] [test: crates/backlog/tests/store_worktree_boundary.rs::sync_apply_from_primary_checkout_is_refused_and_creates_no_issue]。
linked worktree から実行すること。
git が checkout の種別を答えられない場合も拒否する [test: crates/backlog/tests/store_worktree_boundary_env.rs::add_where_dot_git_points_at_a_missing_gitdir_is_refused]。
`list` は primary でも動く [test: crates/backlog/tests/store_worktree_boundary.rs::list_from_primary_checkout_still_works]。
`next --claim` も primary で許可され、`git status` は clean のまま [test: crates/backlog/tests/store_worktree_boundary.rs::next_claim_from_primary_checkout_is_allowed_and_never_dirties_main]。
SessionStart フックは primary では requeue をスキップし、その旨を表示する [test: crates/backlog/tests/store_boundary_hooks.rs::session_start_in_primary_skips_the_requeue_visibly_and_does_not_write]。

### slash command

`/backlog` は queue・state 操作（`list` / `next` / `done` / `fail` / `driver` / `lock`）を呼ぶ薄いエントリポイント。引数でサブコマンドを渡す。

> キューを自動で全件消化したいときは `/backlog` ではなく **`/flow`** を使う。driver 登録 → `next --claim` でピック → `/condukt` → done/fail → 登録解除というループ driver は `/flow` に統合されており、compass ゲート・budgetguard・fugu-router によるモデル選択も含む上位互換 driver になっている。

### SessionStart フックの配線

プラグインとして導入すると、プラグインの `hooks/hooks.json` が SessionStart フック（`backlog session-start`、timeout 10 秒）を登録するので、セッションを開くたびに pending な作業が浮上する。`backlog install` は standalone（cargo）利用向けで、同等の `SessionStart` グループを `~/.claude/settings.json` にマージする（冪等・所有権マーク付き）。`install` / `uninstall` は `--dry-run` で書き込まず結果だけ表示できる。

### 最小例（standalone / cargo）

```sh
cargo install --path .
backlog add --title "Fix X" --project "$PWD" --priority p1 \
  --repro-test "bash tests/repro_fix_x.sh"                  # コミット済みテストが HEAD で落ちた時だけ起票 [test: crates/backlog/tests/repro_evidence.rs::reproduced_lands_pending_and_records_outcome]
backlog list --status pending
backlog next                                                 # 次の項目をピック
backlog done <id> --test "cargo test -p x" --red-rev <rev>  # 解決する（REV で RED・HEAD で GREEN を実測）
backlog done <id> --doc-only <commit>                        # または doc だけの祖先 commit
backlog done <id> --duplicate-of <id>                        # または重複
backlog fail <id> --reason "blocked"                         # 2 日先送りする（2 日後に pending へ戻る）
backlog cancel <id> --reason "やらないと決めた"               # やらないと決めて捨てる（終端。再キューしない。`discard` closure として記録。テスト不要）
backlog lock status                                          # project の排他ロックの保有者を確認
backlog install                                              # SessionStart フックを settings.json にマージ
backlog uninstall                                            # 再び除去する
```

> 補足: `backlog list` の status 語彙は `pending` であり `open` ではない。`list --status open` は何も表示しない。

### テストが無ければ起票しない

- `add` は `--repro-test CMD` が HEAD で振る舞いとして失敗した時**だけ** task を `pending` で書く（再現記録つき） [test: crates/backlog/tests/repro_evidence.rs::reproduced_lands_pending_and_records_outcome]。
- それ以外は非 0 終了で拒否し、store に何も書かない: `--repro-test` 無し [test: crates/backlog/tests/no_confirm_no_unproven.rs::add_without_repro_writes_nothing_anywhere]、再現せず [test: crates/backlog/tests/no_confirm_no_unproven.rs::add_with_non_reproducing_repro_writes_nothing_anywhere]、判定不能 [test: crates/backlog/tests/repro_evidence.rs::undetermined_repro_files_no_row_and_writes_nothing]。
- `next` / `next --claim` は reproduced な repro の無い行を渡さない [test: crates/backlog/tests/no_confirm_no_unproven.rs::next_skips_pending_row_without_repro] [test: crates/backlog/tests/no_confirm_no_unproven.rs::next_claim_skips_pending_row_without_repro]。
- status が `unconfirmed` / `needs-ruling` の行を含む store は `list` / `next` が拒否し、その行を名指しする [test: crates/backlog/tests/no_unconfirmed.rs::list_fails_closed_on_an_unconfirmed_row_in_tasks_toml] [test: crates/backlog/tests/no_unconfirmed.rs::next_fails_closed_on_an_unconfirmed_row_in_tasks_toml] [test: crates/backlog/tests/no_needs_ruling.rs::list_fails_closed_on_a_needs_ruling_row_in_tasks_toml] [test: crates/backlog/tests/no_needs_ruling.rs::next_fails_closed_on_a_needs_ruling_row_in_tasks_toml] [test: crates/backlog/tests/status_type.rs::store_with_unconfirmed_row_fails_naming_id_and_status] [test: crates/backlog/tests/status_type.rs::store_with_needs_ruling_row_fails_naming_id_and_status]。
- `backlog audit-open [--json]` は open 行（pending / failed）を記録済み repro から `has-test` / `no-test` / `undetermined` に分類する [test: crates/backlog/tests/audit_open_once.rs::first_run_classifies_open_rows_and_excludes_terminal]。
- `audit-open` はテストを実行しない [test: crates/backlog/tests/audit_open_once.rs::no_test_is_executed]。
- 判定済みは `~/.backlog/audit-open/<project-slug>.json` に記録される [test: crates/backlog/tests/audit_open_once.rs::first_run_ledger_records_has_and_no_test_but_not_undetermined]。
- 判定済みの行は以後読み飛ばし、後から repro が付いても再確認しない [test: crates/backlog/tests/audit_open_once.rs::row_checked_as_no_test_stays_skipped_after_repro_added]。
- undetermined の行は次回また見る [test: crates/backlog/tests/audit_open_once.rs::second_run_skips_checked_and_reexamines_undetermined]。
- 記録が読めなければ失敗する [test: crates/backlog/tests/audit_open_once.rs::garbage_ledger_is_undetermined_human] [test: crates/backlog/tests/audit_open_once.rs::garbage_ledger_is_undetermined_json]。

### 重複タスクの拒否 (content hashkey)

`add` はタイトルと project から求めた **content hashkey** (`title` を trim → Unicode NFKC → 小文字化 →
連続空白の1個への圧縮 → 前後の記号除去 したものと project を FNV-1a 64bit で畳み込んだ 16 桁 hex)
で内容の重複を検出する。次のいずれかに該当する場合、`add` はエラーで拒否される (`done` の重複はブロックしない
— 同じタイトルを再度積むことは正当なため):

- 同じ hashkey を持つ既存タスクが `pending` または `failed` である。
- `condukt` が PATH 上にあり、`condukt state is-claimed --hashkey <h>` が exit 0 (= 他セッションの
  live なクレームが握っている) を返す。exit 1 は stdout に `"claimed": false` が付いている場合に限り
  「クレームなし」と読む。それ以外 — `condukt` の不在や起動失敗、0/1 以外の exit code (例: 3 = claim registry
  を読めない)、`claimed` フィールドを伴わない exit 0/1、exit 1 に `"claimed": true` (exit と
  claimed の不一致)、シグナル終了、2.5s 以内に答えが返らない — は
  クレーム確認ができなかったことを意味し、`add` は理由を添えて**拒否**される (live な claim 済みタスクの
  重複を排除できないため)。「判定不能」を「クレームなし」とは読まない。

いずれの拒否も `backlog add --force` で意図的にバイパスできる。

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

### store のマージ: `backlog merge-driver`

両 store ファイルは追跡されており、各セッションが自分の worktree で編集するので
`.backlog/tasks.toml` / `.backlog/tasks.done.toml` のマージは日常的に起きる。`.gitattributes` は
両ファイルに `merge=backlog` を指定し、`backlog merge-driver <base> <ours> <theirs>`（git の
`%O %A %B`）が行単位ではなくタスク id 単位でマージする:

- id の和集合をとる。片側で（base に対して）削除され他方で未変更のタスクは削除のまま。片側だけで
  変更されたタスクはその側を採る。
- 同じ id が両側で異なって変更された場合（edit/edit、edit/delete、同じ新 id が異なる内容で追加）は
  **conflict**: どちらも採らず非 0 終了し、`<ours>` は手を付けず、git がファイルを unmerged として
  人間に返す。
- 判定できないもの — パースできない入力、文字列 `id` を持たない行、1 入力内の id 重複、未知の
  top-level キー、マージ結果が再パースでマージ行と一致しない — も非 0 終了で `<ours>` は不変。
  clean な結果はアトミックに書かれる。

**driver は設定された場所でしか動かない。** `.gitattributes` は driver 名を指すだけで、コマンドは
バージョン管理されない git config にある。`merge.backlog.driver` が未設定の repo（や clone）では
git は `merge=backlog` を無視し、通常の行単位テキストマージに落ちる。repo ごとに次で設定する:

```sh
backlog merge-driver --install   # repo 内で実行
```

これは `merge.backlog.name` と `merge.backlog.driver`（実行中の `backlog` バイナリの絶対パス）を
repo のローカル git config（全 worktree で共有）に書く。バイナリが移動・削除されると driver
コマンドが失敗し、git はそれを（黙ったテキストマージではなく）conflict として扱う — `--install` を
再実行すること。

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
- entry (**lease**) は holder の生存が 1h (`CLAIM_STALE_SECS`) 観測されないと除外をやめるので、
  死んだ claimant が全 checkout でタスクを永久にロックすることはない。記録自体は 7 日保持する。
- 「生存の観測」= claim 時刻と holder の最後の lease heartbeat のうち新しい方。`next --claim` は
  claim したセッション (`CLAUDE_CODE_SESSION_ID`) を記録し、`backlog driver heartbeat --session-id S
  --project P` は P の ledger 上の S の **まだ生きている** lease も更新する。したがって 1h 以内ごとに
  heartbeat する driver は、タスクに何時間かかっても claim を保つ (backlog 486e83ca)。期限切れの
  lease は蘇生しない。セッションの無い lease (旧バイナリの entry やセッション外の claim) は従来どおり
  claim 時刻だけで期限切れになる。lease を更新できない (ledger のロック/読み取り/パース/書き込み失敗、
  identity 解決不能) 場合、heartbeat はその旨を出して非0終了する。heartbeat フィールドが壊れていれば
  ledger 全体がパース不能となり、holder を死んだとはみなさず claim を拒否する。

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
- `done`/`fail`/`edit --status pending` は lease を解放しない。終端でないタスクの lease は、holder の
  最後の生存観測から 1h で期限切れになるまで除外を続ける。
- `list`・素の `next`・`next --claim` は同じ方法 (上記の project identity) で ledger を特定する。
  repo 外ではその identity は正規化した cwd。identity を解決できない (例: worktree の `.git` リンクが
  切れている) か、ledger が存在するのに読めない・パースできない場合、`list` と素の `next` は
  **拒否** する (非0終了、理由は stderr、stdout は空)。claim 済みかもしれないタスクを pending と
  表示することはない。
- store 乖離チェックは lease 中のタスクを「キューに残る作業」に数えない (以前、保存された
  `claimed` 行を数えなかったのと同じ)。

git で追跡しているのは `bin/backlog` ランチャーだけである。ランチャーは配布先プラグインで隣に置かれたプラットフォーム別の `backlog-<os>-<arch>` バイナリを exec し、ホスト用のビルドが無ければ（空出力で exit 0 ではなく）非 0 で終了する。変更が稼働ハーネスに届くのは `scripts/rollout-plugins.sh` を通したときだけ（`docs/repo-operations.md` 参照）で、エンドユーザーは cargo も API キーも不要。
