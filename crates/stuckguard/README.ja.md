# stuckguard

Claude Code 向けの、行き詰まりループ検出 + エスカレーション。Rust 製。

## 目的

stuckguard は、エージェントが同じ失敗を繰り返したり、編集を行ったり来たり堂々巡りしている状態を決定論的に検出し、アプローチを変える／ユーザーに助けを求めるよう促す **PostToolUse** フックである。

ツール呼び出しのストリームを監視し、次の 2 種類のループを検出する。

| シグナル | 発火条件 |
|---|---|
| **repeat（反復）** | 直近のウィンドウ内で、正規化された同一の `(tool, input)` が `repeat_threshold` 回走る（例: 同じ `cargo test` を 3 回）。各回がいずれもエラーしていた場合は「（毎回失敗しています）」を付す。 |
| **oscillation（振動）** | 編集のスラッシング。あるファイルが X→Y のあと Y→X と繰り返し編集される（`oscillation_threshold` 回の反転）、つまり変更が取り消されては再適用されている状態。 |

repeat 検出には**近似反復のマッチング**もある。`similarity_threshold` を `1.0` 未満に設定すると、同一ツールへの 2 回の呼び出しは、正確なシグネチャが異なっていても Jaccard トークン集合類似度がしきい値を超えた時点で反復とみなされる。既定値 `1.0` では従来どおり完全一致のみを反復として扱う（挙動は変わらない）。

repeat/oscillation によるハードなエスカレーションのさらに手前には、任意（既定オフ）の**progress-score アドバイザリ**がある。`progress_advisory_enabled` で有効化すると、直近ウィンドウにおける 3 つのシグナル — アクションの多様性・状態ハッシュの安定性・エラーダイジェストの再発 — を組み合わせて `[0, 1]` の `progress_score` を算出し、ウィンドウが `progress_min_window` 件以上かつスコアが `progress_score_threshold` 以上になると、早期の「進捗が停滞しているかもしれない」というナッジを注入する。ハードなエスカレーションを置き換えたりブロックしたりすることはない。

さらに最下位の任意アドバイザリとして、**scope-drift（スコープ逸脱）検知**（PDO session-anchor、DESIGN §4.4）がある。`scope_drift_enabled`（既定オフ = opt-in）で有効化すると、`watch` は現在セッションの live な overwatch lease（`overwatch lease --session <id> --json`）から宣言済み scope を読み、直近の連続する EDITED ファイル（Edit/Write。既存シグネチャの file_path で追跡）が `drift_threshold`（既定 3）回連続で scope の**外**に落ちたとき、「anchor を更新するか元のタスクに戻れ」という助言ナッジを出す。ハードなエスカレーションと progress アドバイザリとは**構造的に排他**（else-if チェーン）で、ハードトリップが常に優先し、scope-drift は最下位で両者の bookkeeping を一切乱さない。セッションが lease を持たない・scope が空・overwatch が無い場合は no-op（fail-soft）。助言のみでブロックしない。

長い単一タスクが claim/lease を失効させて誤って reap/奪取されないよう、**heartbeat piggyback**（DESIGN §4.6b）も持つ。`heartbeat_piggyback_enabled`（既定オン = 安全機能）が有効なら、`watch` は PostToolUse のたびに（セッションの live lease から解決した）`condukt state heartbeat --run <rid>` と `overwatch heartbeat --key <k>` を発火し、タスク実行中も claim/lease を生かし続ける。live lease が無い・バイナリが無い場合は no-op（fail-soft）で、ナッジ経路を決してブロックしない。

stuckguard が行うのは **助言の注入だけ**である。ツール呼び出しをブロックすることも、ターンを終了させることもできない。そのため誤検出のコストは余計な 1 行のコンテキストにとどまる。API キーは不要だ。

## どうして必要か

エージェントは行き詰まる。失敗するコマンドを延々と再実行したり、収束しないままファイルを編集して回り続けたりする。Devin のハーネスが持つ「自信のなさを認めて助けを求める」反射神経が、LLM 単体には備わっていない。放置すると、モデルは堂々巡りに気づかないままトークンと時間を浪費し続ける。

stuckguard はこの失敗モードを、言語モデルの自己観察ではなく決定論的な検出で捉える。ツール呼び出しの履歴からループのパターンを機械的に見抜き、まず「一歩引いて別のアプローチを試せ」と促し、それでも繰り返すなら「いったん止めてユーザーに尋ねよ」へとエスカレートする。小さなローカルバイナリとして、その反射神経をハーネスに足すものだ。

## どう使うか

stuckguard は単一の Rust 実行ファイルで、ジョブごとに 1 つのサブコマンドを公開する。`watch` サブコマンドが **PostToolUse** フックに配線され、各ツール呼び出しのたびに次を行う。

1. 呼び出しから安定した**シグネチャ**を作る（コマンドの正規化、編集ならファイル名 + before/after のハッシュ）。`DefaultHasher` によりプロセスをまたいで決定論的。
2. セッションごとの**リングバッファ**（`window` 件のイベント）をディスクに追記する。
3. ウィンドウに対して検出器を走らせる。oscillation が repeat に優先する。
4. 発火したら、そのパターンが**クールダウン**中でない限り、`additionalContext` 経由でナッジを注入し、当該パターンのナッジ回数を増やす。
5. あるパターンが `escalate_after` 回ナッジされると、メッセージは明示的な**「止めてユーザーに尋ねよ」**へ昇格する。

状態はすべてローカルに置かれる（`~/.stuckguard/state/`）。ナッジ 1 件につき `log.jsonl` に JSONL 1 行が記録される。

記録される各イベントは `failed_test_digest` も保持する。ツール呼び出しがエラーした場合、正規化したエラーテキスト（パス・行番号・アドレスは除去済み）の決定論的ハッシュである。これは上記のエラーダイジェスト再発シグナルに使われるほか、プロジェクト横断の lessons ストアに対する検索キーも兼ねる。エスカレーション時には、stuckguard はこの行き詰まりパターンについてエラーパターンのレッスンを書き込み、関連する過去のレッスンを検索してエスカレーションメッセージに含める — いずれも fail-soft（lessons ストアが欠落・破損していてもナッジ自体はブロックされない）。

### セットアップ

```sh
cargo install --path .
cd your/project
stuckguard init        # 任意: 雛形となる stuckguard.toml を書き出す
stuckguard install     # PostToolUse フックを ~/.claude/settings.json にマージする（バックアップを取る）
stuckguard status      # 解決済みの設定を表示する
```

解除は `stuckguard uninstall`。一時無効化のキルスイッチは `STUCKGUARD_DISABLE=1`。

設定は [`stuckguard.example.toml`](stuckguard.example.toml) を参照。主なキーは次のとおり。

| キー | 意味 | 既定値 |
|---|---|---|
| `window` | セッションごとに検査する直近のツールイベント数 | 12 |
| `repeat_threshold` | ウィンドウ内で同一アクションが何回でナッジするか | 3 |
| `similarity_threshold` | 同一ツールへの 2 回の呼び出しを近似反復とみなす Jaccard トークン集合類似度（`[0, 1]`）。`1.0` は完全一致のみ（従来の挙動） | 1.0 |
| `oscillation_threshold` | 1 ファイルの編集反転が何回でナッジするか | 2 |
| `cooldown_events` | あるパターンを再ナッジしないイベント数 | 6 |
| `escalate_after` | 「ユーザーに尋ねよ」へ昇格するまでのナッジ回数 | 2 |
| `ignore_tools` | 検出対象から除外するツール | `["TodoWrite"]` |
| `progress_advisory_enabled` | ハードな repeat/oscillation エスカレーションより手前で発火する、3 シグナルによる progress-score アドバイザリを有効化する | false |
| `progress_min_window` | このアドバイザリが発火しうる最小のウィンドウ長 | 6 |
| `progress_score_threshold` | アドバイザリが発火する `progress_score`（`[0, 1]`）のしきい値 | 0.75 |
| `scope_drift_enabled` | 直近の編集がセッションの宣言済み anchor scope（overwatch lease）の外に落ちたときに発火する scope-drift アドバイザリを有効化する（§4.4。overwatch 必須、fail-soft） | false |
| `drift_threshold` | scope-drift が発火するまでの連続 scope 外編集数（1 以上に floor、window に clamp） | 3 |
| `heartbeat_piggyback_enabled` | PostToolUse ごとに condukt/overwatch の heartbeat を更新し claim/lease の失効・誤 reap を防ぐ（§4.6b。live lease が無ければ no-op） | true |

サブスクリプションで完結し（フック 1 つ + 同梱の Rust バイナリ）、`ANTHROPIC_API_KEY` も追加インストールも不要である。

## ライセンス

MIT
