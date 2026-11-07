# donegate

> **Claude Code 向けの完了検証ゲート**。Rust 製。

エージェントは「完了した」と早すぎる宣言をしがちだ。`donegate` は、その「完了」を*検証済み*の意味に変えるハーネスである。**Stop** のたびに、受け入れコマンド（`build` / `test` / `lint` / `typecheck` など）を**サブプロセス**として実行し、必須チェックがすべてグリーンになるまでターンの終了を許さない。失敗したチェックの出力の要点が会話に差し戻される（全文は session 記録に保存）ので、エージェントは赤のまま停止するのではなく**自動修正ループ**に入る。

## 目的

donegate は、Claude Code の **Stop フックに配線された決定論的な Rust バイナリ**だ。停止のたびにプロジェクトの受け入れコマンドを実プロセスとして起動し、終了コードを読むことで「本当にビルドが通り、テストが通るのか」を判定する。

役割は明確に分けられている。

- 必須チェックがすべてグリーン → 終了コード 0 を返し、停止をそのまま通す。
- 必須チェックのいずれかが失敗 → `{"decision":"block","reason":…}` を出力して停止をブロックし、失敗したコマンドと出力の要点（全文は session 記録ファイル、そのパスを表示）を理由に含めて Claude に作業を続けさせる。

これは 2 つの静的ゲートに対する*動的*な兄弟分にあたる。

| ゲート | タイミング | 問い |
|---|---|---|
| `precommit-audit` | pre-commit | diff はポリシーに従っているか?（静的） |
| `specguard` | オンデマンド | 実装が仕様からドリフトしていないか?（LLM） |
| **`donegate`** | **Stop 時** | **実際にビルドが通り、パスするか?（実行して確かめる）** |

LLM の労働（失敗の修正）は Claude Code のサブスクリプション内で行われる。donegate 自体はプロセスを起動して終了コードを読むだけの決定論的バイナリなので、**API キーは不要**で、レートリミットに当たることもない。

## どうして必要か

LLM エージェントは、コードを書いただけでタスクを「完了」と判断して停止してしまうことがある。実際にはビルドが壊れていたり、テストが赤だったり、lint が通っていなかったりしても、目視確認だけでは見落とされる。この失敗モードを人間のレビューだけで塞ごうとすると、エンジニアが毎回手元でビルド・テストを回して確認する羽目になる。

donegate は、この「未検証のまま完了宣言する」問題を停止の瞬間に物理的に止める。受け入れ基準を満たさない限りターンを終わらせないため、検証は LLM の自己申告ではなく、実際のプロセスの終了コードという決定論的な根拠に基づくものになる。

無限ループにエージェントを閉じ込めないための安全装置も組み込まれている。

- **試行カウンタ**: セッションごとに連続ブロック回数を数え、`max_attempts`（既定 2）回に達したら諦めて停止を許す。本当に詰まったエージェントが罠にかからないようにするためだ。カウンタは `reset_after_secs` のアイドル後、またはグリーンになった時点でリセットされる。
- **エスケープハッチ**: `donegate skip --reason "<理由>"`。一度だけ消費されて次の停止を通し、**発行したセッションにだけ**適用される。理由は必須で、発行と消費の両方がゲートログに追記されるため、記録の残らないバイパスは起こらない。（旧来の project root の `.donegate-skip` ファイルは撤去した。共有の 1 回限りマーカーは次に停止したセッションが消費してしまい、その*別*セッションの正当なゲートを素通りさせる — CLAUDE.md §5 が並行セッション下で禁じているのはまさにこれ。）
- **キルスイッチ**: `DONEGATE_DISABLE=1`。ただし **Claude Code 自身を起動した環境**でのみ有効で、ツール呼び出しから export しても、このフックはアプリの環境を継承するため届かない。
- **オプトイン**: 設定ファイルが 1 つも無い（`<root>/donegate.toml` も `~/.donegate/config.toml` も無い）か、読み込んだ設定に `[[check]]` が 1 つも無ければ、ゲートはすべての停止を許す。フックを入れただけで、オプトインしていないプロジェクトをブロックすることはない。セッションが書き込んだ root がすべて donegate の設定を一切持たない場合も停止は許可され、stdout は空になる（`systemMessage` は出さない。2026-10-27 のユーザー裁定）。代わりに stderr に「donegate not configured for <roots>: no checks ran」を出し、`<state_dir>/log.jsonl` にセッションと各 root（`roots`）を記した `"verdict":"unconfigured"` の行を追記する。
- **判定できないときはブロックする**: project の `donegate.toml` があるのに root が**未信頼**（実行を拒否）、設定ファイルが存在するのに読めない・解析できない、宣言されたチェック名が予約名前空間 `undetermined:` に入っている — これらは許可ではなくブロックになる。同じ `max_attempts` カウンタで有界で、使い切ると「NOTHING WAS VERIFIED」と明示して停止を通す（`giveup-refusal` として記録）。
- **donegate 自身の panic** は `harness_core::gate::run::run_guarded` を通り fail-closed（最初の停止はブロック。`stop_hook_active` の再入で 2 回連続 panic したときだけ通す）。

## どう使うか

donegate はプラグインに同梱された 1 つのバイナリとフック群（Stop / SubagentStop、および Bash の PreToolUse / PostToolUse / PostToolUseFailure）だけで動く（サブスクリプション完結・API キー不要）。フックは `${CLAUDE_PLUGIN_ROOT}/bin/donegate gate` を Stop に、`${CLAUDE_PLUGIN_ROOT}/bin/donegate subagent-gate` を SubagentStop に、`donegate dup-test pre|post` を Bash の前後に配線する。

CLI を手動で使う場合の典型的な流れ:

```sh
cargo install --path .
cd your/project
donegate init          # スタックを自動検出してスターター donegate.toml を生成
donegate trust         # このプロジェクトの donegate.toml を信頼する（プロジェクト設定は信頼ゲート付き）
donegate install       # Stop / SubagentStop フックを ~/.claude/settings.json にマージ（バックアップを取る）
```

その後、何が走るかを確認する:

```sh
donegate status        # 解決済みの設定 + 現在の diff に適用されるチェック
donegate gate          # チェックを手動実行（人間向けレポート、失敗時は exit 1）
```

主なサブコマンド:

| サブコマンド | 役割 |
|---|---|
| `donegate gate` | Stop フック本体。適用対象のチェックを実行し、グリーンになるまで停止をブロックする。 |
| `donegate subagent-gate` | SubagentStop フック本体。停止する subagent が書いた root のチェックを実行し、グリーンになるまでその停止をブロックする。 |
| `donegate init` | プロジェクト種別（Cargo / npm / Python / Go）を自動検出して `./donegate.toml` を生成する。 |
| `donegate install` / `uninstall` | `~/.claude/settings.json` への Stop / SubagentStop フックの追加 / 削除。 |
| `donegate status` | 解決済みの設定と、現在の作業ツリーに対して実行されるチェックを表示する。 |
| `donegate trust` | 現在のプロジェクトをワークスペース信頼リストに追加し、そのプロジェクト固有の `donegate.toml` のコマンドを Stop 時に honor させる。 |
| `donegate dup-test pre` / `post` | PreToolUse / PostToolUse(Bash) フック（下記「重複 cargo test の拒否」）。 |

### 重複 cargo test の拒否（dup-test フック）

`hooks/hooks.json` は Bash に `donegate dup-test pre`（PreToolUse）と `donegate dup-test post`（PostToolUse と PostToolUseFailure）も配線する。目的は「対象テストは 1 回だけ走らせる」の機械化で、**同じコマンドが同じツリーで既に PASS している `cargo test` の再実行を deny する**。

- 対象は単一の素の `cargo test ...` だけ。`&&` `;` `|` `(` `$` クォート・リダイレクト・改行・環境変数代入・`cd`・`cargo +toolchain`・`--manifest-path`・`--config` を含むものは判定も記録もしない（常に通す）。
- キーは canonical な cwd + git toplevel + 空白正規化した argv + passcache と同じツリー状態（HEAD・`main`・一時 index に `git add -A` して `write-tree` した作業ツリー内容。未追跡ファイルも含む。ignore されている未追跡ファイル〈`target/` 等〉・リポジトリ外・環境変数はキーに含まれない）。
- `pre` はキーを計算し、そのキーの PASS 記録があれば `permissionDecision: "deny"` を返す（理由: 同じコマンドがこのツリーで既に PASS したので、本当に再実行したいならツリーかコマンドを変えること）。無ければ `tool_use_id` ごとにキーを退避する。
- `post` は `hook_event_name == "PostToolUse"` かつ `tool_response.interrupted == false`（Bash の tool_response には exit code が無く、失敗は `PostToolUseFailure` で届く）かつ、`pre` が退避したキーと post 時点のキーが一致したときだけ PASS を記録する。
- 疑わしいときは常に「deny しない・記録しない」に倒す: git 外・git エラー・ペイロード不正・ストア破損・未知のイベント。
- ストア: `~/.donegate/dup-test/`（`passes/`・`pending/`・`tmp/`）。`DONEGATE_DISABLE=1` で無効。
- コスト: 全 Bash 呼び出しで走るが、素の `cargo test` 以外は stdin の JSON パース直後に戻る（ファイル・git アクセス無し）。

プロジェクトの `donegate.toml` は、そのプロジェクトが信頼済み（`donegate trust`）になって初めて honor される。これにより、敵対的なリポジトリが Stop 時に任意のコマンドを紛れ込ませることを防ぐ。未信頼のプロジェクトファイルの代わりに `~/.donegate/config.toml` のチェックを使うことは**しない**：`donegate trust`（または `HARNESS_TRUST_ALL=1`）するまで、実行拒否として停止をブロックする（有界、上記参照）。home 設定から取るのはつまみ（`enabled`・`max_attempts`・`state_dir`・タイムアウト）だけ。リポジトリの main 作業ツリーを信頼すれば、その linked git worktree も信頼される。

### gate の動作

`donegate gate` は Stop ごとに次を行う。

1. フックの JSON を stdin から読む（`session_id`, `cwd`, `stop_hook_active`）。
2. `./donegate.toml`（または `~/.donegate/config.toml`）を読み込む。
3. `git` に変更ファイルを尋ね、`when_changed` のグロブが一致するチェックを選ぶ（スコープなしのチェックは常に走る）。
4. 選ばれた各チェックをサブプロセスとして、チェックごとのタイムアウト付きで実行し、結合出力を一時ログへ流して、読み戻すのは**一定量の窓だけ**にする。

#### pass キャッシュ

**通った（pass した）**チェックの結果だけを `<state_dir>/pass-cache/` に記録し、キーが 1 バイトも変わっていなければ次の Stop（親 Stop が subagent の root を再判定する場合も含む）では再実行しない。キーは root パス・チェック名・`cmd` 文字列・`workdir`・実効タイムアウト・`HEAD` の sha・`main` が指す sha（無ければ「無い」と確定した値。変更集合の merge-base はこの 2 つで決まる）・作業ツリーの内容。作業ツリーの内容は、空の一時 index（`GIT_INDEX_FILE`、`<state_dir>/tmp` 配下）に `git add -A`（tracked＋ignore されていない untracked）と、本物の index が追跡している ignore 対象パスの強制 add をしてから `git write-tree` した tree。本物の index は読むだけで触らない（副作用として blob が object store に書かれる）。

- キーが計算できない（repo でない・git の失敗・読めないファイル）・ストアが読めない／壊れている・保存されたキーと違う、はすべて **miss** で、チェックを実行する。
- 失敗したチェックは保存しない。そのチェックのエントリは消す。
- 実行の前後で tree が変わったら（チェックが tree を書き換えた・並行編集）、その回の pass は保存しない。
- hit は黙らない。stderr に `CACHED pass … (pass cache)` と出し、人間向けレポートでも `CACHED pass from <時刻>` と表示する。
- キーに含まれないもの: 追跡されていない ignore 対象ファイル（`target/` など）、repo 外のファイル、チェックが継承する環境変数。
- give-up・試行回数・waiver の扱いは変わらない（hit は pass と同じ扱い）。

#### 実行中のバックグラウンド subagent の root（SubagentStop）

実行中のバックグラウンド subagent **だけ**が書いた root は、親の Stop では判定しない。subagent がまだ編集中なので、判定すると推論中の編集と競合する。その root は黙って飛ばさず名指しする。stderr に出し、Stop の `systemMessage` またはブロック理由にも `[root: …] NOT judged at this stop: only background subagent(s) <ids> wrote it …` として載せる。「実行中」と判定するには次の 3 つがすべて要る。

- `agent-<id>.meta.json` に `toolUseId` がある。
- いずれかの transcript（親か sidechain。入れ子の起動元も含む）に、対応する `toolUseResult {isAsync: true, status: "async_launched", agentId}` がある。
- どの transcript にもその agent の `<task-notification>`（`task-id`）がない。

次のいずれかに当たれば、親の Stop は従来どおりその root を判定する。

- 状態が判定できない。
- subagent が終了済み。
- 親自身、または実行中と観測できない他の subagent もその root に書いた（cwd がその root 内の Bash 呼び出しを含む）。
- その root が hook cwd である。
- transcript を読めない。この場合は「実行中 root を判定できず、全 root を判定した」という通知を同じ経路で出す。

`donegate subagent-gate` は **SubagentStop** フックで、`hooks/hooks.json` と `donegate install` が登録する。停止する subagent が書いた root を判定する。その subagent の transcript は `agent_transcript_path`、無ければ `<transcript dir>/<stem>/subagents/agent-<agent_id>.jsonl` から読む。判定は親の追加 root と同じロジックで行う。

- green なら許可する。
- red なら `{"decision":"block"}` で subagent の停止をブロックする。
- 同一指摘の再発は `systemMessage` 付きで見送る。
- `max_attempts` に達したら give-up する。

カウンタと見送り記憶は `<session>-agent-<agent_id>` をキーにするので、親の連鎖とは混ざらない。何も書かなかった subagent は黙って許可する。config が無い root やチェックを宣言していない root は、ここでは判定しない。次の場合はブロックする。

- donegate がその root のチェック実行を拒否した。
- agent transcript の場所が分からない、または読めない。
- root を判定できない。

`donegate skip` は親セッションのものなので、SubagentStop では消費しない。subagent が終了した後は、親の Stop が改めてその root を判定する。

#### 失敗出力の session 記録と静かな表示

- 失敗したチェックの生出力全体は、バイト単位でそのまま
  `<state_dir>/records/<session>/<unix-ms>-a<attempt>-<check>.log` に保存される
  （`<session>` は sanitize した session id。空なら `nosession`）。保持は最新 20 session
  ディレクトリ・1 session あたり最新 50 件。現在の session のディレクトリは削除しない。
  保持処理の失敗（not found 以外）は保存を取り消さず、記録パスの下に
  `record retention failed: …` 行として表示する（握りつぶさない）。
- block 理由に表示するのは判断を促すものだけ: チェック名・exit code・コマンド、
  cargo 形式なら各 `panicked at` 行とそのメッセージ、`failures:` の名前一覧、
  `test result: FAILED`、診断行、最終行の FAILED。それ以外の出力は診断行、主要なテストランナーの
  失敗行（`FAILED` を含む行、go の `--- FAIL: …`、python unittest の `FAIL: …`、jest / go の
  `FAIL <path>`。最大 10）、末尾 5 行。診断行とは `error[…]`/`error:`/`warning:` 見出しとその最初の
  `--> file:line:col` 位置行（最大 5 件、error 優先。同じ見出しでもファイル・位置が違えば位置ごとに残す）、
  位置を行内に持つ 1 行診断（tsc の `path(3,5): error TS2322: …`、gcc/clang の
  `path:3:5: error: …`。同じ上限 5 件に数える）、位置を持たない `error:` 要約行
  （`could not compile` 等）、およびコンパイラの件数要約（tsc の `Found N errors…`、
  gcc/clang の `N errors generated.`）。1 チェック 24 行が上限で、空にはならない。そして**記録ファイルの絶対パス**。
  生の `--- stdout ---` / `--- stderr ---` 節や進捗行は表示しない。
- 記録を保存できないときもチェックは実行し、停止もブロックする。理由に「保存できなかった」旨と
  原因を明記し、表示は**減らさず増やす**（digest に加えて末尾
  `max(output_tail_lines, 40)` 行の生出力）。
- repeat ledger は、試行ごとに変わる記録パスを正規化した理由をキーにする（試行カウンタ
  `attempt N/max` はキーに残る）。
- **同一所見の waiver**（backlog b80023b3）: 連続ブロックの 1 チェーン内で、root のツリー
  （HEAD・追跡差分・未追跡ファイル）と失敗チェック出力が前回ブロック時と同一の Stop は再ブロックせず、
  「新しく検証したものは無い・チェックはまだ RED」と書いた JSON `systemMessage` 付きで通す。waiver した
  停止は `max_attempts` に数えず、ツリーが変わればブロックする。適用は `session_id` が空でない Stop hook
  payload のみ（手動 `donegate gate` は exit 1 のまま、session id の無い payload はブロック）。記憶は
  チェーン終了（グリーン・give-up・skip・`reset_after_secs` のアイドルリセット）で消える。これとは別に、
  共有の `harness_core::repeat` ledger（2026-09-18 裁定）は同一セッション内で同文のブロックが再発すれば
  通しうる。ツリーや署名が判定できなければブロックする。waiver は
  gate log に `repeat-waived` として残るが overwatch ledger には書かない。別 root がブロックするときは、
  他 root の waiver / give-up 通知をブロック理由に追記する。有界な出口は試行上限と give-up sentinel
  （`donegate:giveup:<check>`）である。
5. すべてグリーンなら exit 0 で停止を通し、いずれか必須が失敗したら `block` を出して Claude に作業を続けさせる。

### 設定

[`donegate.example.toml`](donegate.example.toml) を参照。各 `[[check]]` のキー:

| キー | 意味 |
|---|---|
| `name` | ブロック理由に表示されるラベル |
| `cmd` | シェルコマンド（`sh -c` / `cmd /C`） |
| `when_changed` | `git diff HEAD` + 未追跡ファイルに対するグロブ。省略すると常に実行 |
| `timeout_secs` | チェックごとのタイムアウト（既定は `default_timeout_secs`） |
| `optional` | `true` の場合、失敗時は警告のみでブロックしない |
| `workdir` | プロジェクトルート配下のこのサブディレクトリで実行する |

### ログ

すべての判定は `~/.donegate/state/log.jsonl` に JSONL 1 行として追記される（`verdict` は `green` / `blocked` / `giveup` / `giveup-reentry` / `skip` / `refused` / `giveup-refusal` / `giveup-refusal-reentry` のいずれか）。ローカルのみ。give-up は overwatch の violation ledger にも記録される（`donegate:giveup:<check>` / `donegate:undetermined:<kind>`）。

## ライセンス

MIT
