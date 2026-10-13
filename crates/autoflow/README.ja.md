# autoflow

> 🌐 [English](README.md) ・ **日本語**

セッション終了時の auto-flow ゲート — 走っている condukt run を抱えたままセッションが終わるのを防ぐ **Stop** フックと、`/compact` を跨いでもループを継続させる **PreCompact** + **UserPromptSubmit** の組。

## 目的

autoflow は、ターンが終わったときに「まだ片付いていない仕事」が残っていればセッションの終了をブロックする Stop フックである。具体的には、ターン完了時にまず一度だけ `/record`（`/session-insights:record`）を促し、続いて**現在の condukt run** の保留タスクが片付くまで `/condukt` をループで促す。保留が無いことを観測したときに限り黙る（run-state を読めなかった場合は黙らずにブロックする — 後述）。

**backlog キューは対象外である（2026-08-20 に廃止）。** それ以前は、condukt の保留が空になると backlog キューを読み、未消化アイテムがあれば `/backlog を実行してください` で毎ターン stop をブロックしていた（compass charter が陳腐化していれば代わりに `/compass` を促して撤退）。同じ要求を **SessionStart** フックが `バックログに N 件 (最優先: '…')。/flow で開始しますか？` としてセッション冒頭でも出していた。**この3経路（SessionStart 提案 / Stop の backlog アーム / その compass ゲート）はユーザーの指示で撤去した**。キューを消化するかどうかは操作者の判断であり、`/flow` や `/backlog` を明示的に起動したときだけ行われる。backlog クレート自身が SessionStart でキューの状態を注入するのは変わらない — 無くなったのは「それを今やれ」という指示だけである。

中身はセッションごとの状態機械で、各フェーズが「ターンをブロックして `/` コマンドで誘導するか」「そのまま終了させるか」を決める。

| フェーズ | 条件 | autoflow の動作 |
|---|---|---|
| **Idle** | このセッションで十分なターン数とツールイベントがある | block → `/session-insights:record` |
| **RecordRequested / Continuing** | condukt タスクがまだ保留中 | block → `/condukt`（保留集合が縮んでいる限り継続。回数上限は無く、保留集合が `stuck_threshold`〈既定 3〉回連続で縮まなかったときだけ可視のエスカレーション） |
| **RecordRequested / Continuing** | condukt の run-state を読み取れない | 「判定不能」の理由付きで block。フェーズは更新しないので次の Stop で観測し直す |
| **Done** | condukt run に保留タスクなし | ターンの終了を許可（backlog キューは見ない） |

継続は回数ではなく**進捗**で決まる: 保留集合が縮んでいる限りブロックし続け、`stuck_threshold` 回連続で縮まなければ**可視の**エスカレーション（自律モードなら継続を宣言、非自律ならユーザーに確認）に切り替える。黙って撤退はしない。自律かどうかは共有の autonomy スイッチ（`harness_core::autonomy`）から読み、文言を変えるだけでどちらもブロックする。エスカレーションする Stop だけは保留タスクを `running` に再マークしない。

また `backlog lock status` がこのプロジェクトの生きた driver を報告している間は autoflow は完全に撤退し（Stop を許可し）、稼働中の `/flow` や `/backlog` driver を二重に駆動しない（この撤退は促しではなく譲歩なので残してある）。この撤退の向きに注意: liveness が判定できなかった場合（backlog の実行失敗・非0終了・解釈不能な出力・インストール有無の判定不能）も「driver がいるかもしれない」として撤退する — 二重駆動を防ぐ代わりに、その Stop では促さない。backlog が*観測上*インストールされていない場合は撤退しない。

さらに毎回の Stop で次の 2 つを検査する:

- **ユーザーの明示的な停止**（状態機械より前）: transcript 上でユーザーの直近のシグナル（入力したメッセージ、または `AskUserQuestion` への回答）が明示的な停止指示（「止めて」「stop」等）なら、*その* Stop は何もせず通す。`Done` には latch しないので、ユーザーが作業を続ければ促しは再開する。読めない・無いシグナルを停止とみなすことはない。
- **委譲記録の advisory**（RecordRequested / Continuing 中）: このセッションの transcript で `/flow` が condukt run を完了させたのに `fugu-router record` が呼ばれていなければ、セッションにつき一度だけ `fugu-router record --class flow-delegation …` を促してブロックする。

**PreCompact** では、このセッションが `backlog lock status` 上で排他ロック保持者または登録済み driver の一つであり（＝実際にこのセッションから `/flow` ループを駆動中。status を読めない場合は「駆動していない」扱い）、ユーザーが opt-out（`resume_flow_on_compact = false`）していなければ、resume マーカーを書き込む — compaction 自体をブロックすることはない。続く **UserPromptSubmit** はそのマーカーを（あれば）消費し、「`/flow` を再開せよ」という指示を一度だけ注入する。マーカーが無い通常のターンでは常に黙る。

サブスクリプションネイティブな設計で、3 つのフックと同梱の Rust バイナリだけで動き、**API キーは不要**、デーモンも不要。Stop フックは理由付きの `block` 判定を出すだけで、作業そのものを実行することはない（副作用は自身のセッション状態ファイル、通常の継続時に保留 condukt タスクを `running` にマークすること、block ごとの overwatch violation 記録のみ）。判定を持つゲートなので「判定不能」を「許可」には倒さない: condukt の run-state が読めなければ**ブロック**し、Stop フック内の panic は `harness_core::gate::run::run_guarded` により fail-closed で一度 `block` する（直後の `stop_hook_active` の再入だけは通すので、クラッシュでセッションが閉じ込められることはない）。ホスト向けバイナリが無い場合も launcher が `stop` に対して `block` を出す。黙って終わるのは: セッション ID が無い（payload にも `CLAUDE_CODE_SESSION_ID` にも無い）場合、ゲートが無効化されている場合、condukt の run ファイルが存在しない場合（condukt がこのプロジェクトで走ったことが無い＝「保留タスク無し」の観測）、上記の撤退条件の場合である。

## どうして必要か

長いセッションは、record を取り忘れたり、走らせた condukt run の保留タスクを残したまま終わりがちである。これらは「ターンが終わった」というだけの理由で人間にもエージェントにも気付かれず、床に落ちたまま忘れられる。autoflow はセッション終了という決定論的なタイミングに「やり残し検査」を割り込ませ、record → condukt の連鎖を確実に回す。

**「やり残し」に backlog キューを含めない**のは 2026-08-20 の方針転換である。走っている run の保留タスクは*このセッションが始めた仕事*なので放置は事故だが、backlog キューは*まだ始めていない仕事*であり、それを毎ターン催促するのは検出ではなく催促にすぎない。促しの回数は情報量を増やさないので、撤去した。

判断（どう作業するか）は引き続き各スキルと LLM が担い、autoflow が担うのは「終わらせてよいか」のゲートだけである。暴走への歯止めは回数ではなく進捗でかける: 保留集合が縮まないまま `stuck_threshold` 回続いたら可視のエスカレーションに切り替え（非自律ならユーザーに判断を委ね、自律なら継続を宣言する）、他セッションがロックを握っていれば手を引く。停滞時に黙って撤退する経路は無く、状態機械が自ら許可する停止は保留集合が空になったときだけである（これとは別に、ユーザーの明示的な停止指示と、他 driver への譲歩でもその Stop は通る）。

## どう使うか

プラグインマーケットプレイス経由でインストールすると、同梱の `hooks/hooks.json` が **Stop**・**PreCompact**・**UserPromptSubmit** の 3 フックを `${CLAUDE_PLUGIN_ROOT}/bin/autoflow` に自動配線する（**SessionStart** は 2026-08-20 に撤去）。ほかに設定は要らず、ゲートはデフォルトで有効。しきい値（最小ターン数・最小ツールイベント数・停滞しきい値・resume-flow-on-compact）は config のデフォルト値から来る。上書きするには `~/.autoflow/config.toml` を書く:

```toml
enabled = true                # 既定 true
min_turns = 2                 # /record を促すまでのターン数
min_tool_events = 3           # /record を促すまでのツールイベント数
stuck_threshold = 3           # 可視エスカレーションまでの無進捗 Stop 回数
resume_flow_on_compact = true
state_dir = "~/.autoflow/state"
```

パースできない config ファイルは無視される（既定値が使われる）。

スタンドアロン（cargo）で使う場合:

```sh
cargo install --path .
autoflow stop            # Stop フック: record→condukt の状態機械を実行
autoflow pre-compact     # PreCompact フック: このセッションがロックを保持していれば resume マーカーを書く
autoflow prompt-submit   # UserPromptSubmit フック: マーカーを消費して「/flow を再開」を一度だけ注入
```

`autoflow stop` は stdin でフック JSON を読み、`block` 判定を出力する（または何も出力しない）。`autoflow pre-compact` と `autoflow prompt-submit` は resume マーカーのゲートが成立しない限り黙る。`AUTOFLOW_DISABLE=1` でゲートを無効化できる。

コミットされているのは `bin/autoflow` launcher だけで、これがプラットフォーム別の `bin/autoflow-<os>-<arch>` を exec する。後者は gitignore されており、リポジトリの `scripts/rollout-plugins.sh` が配置する（`docs/repo-operations.md` 参照）。エンドユーザーは cargo も API キーも不要。フックが依存する挙動を変えたときは rollout をやり直す。テストは `cargo test` で実行する。
