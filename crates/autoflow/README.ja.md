# autoflow

> 🌐 [English](README.md) ・ **日本語**

セッション終了時の auto-flow ゲート — 走っている condukt run を抱えたままセッションが終わるのを防ぐ **Stop** フックと、`/compact` を跨いでもループを継続させる **PreCompact** + **UserPromptSubmit** の組。

## 目的

autoflow は、ターンが終わったときに「まだ片付いていない仕事」が残っていればセッションの終了をブロックする Stop フックである。具体的には、ターン完了時にまず一度だけ `/record`（`/session-insights:record`）を促し、続いて**現在の condukt run** の保留タスクが片付くまで `/condukt` をループで促す。保留が無ければ黙るので、ターンを壊すことはない。

**backlog キューは対象外である（2026-08-20 に廃止）。** それ以前は、condukt の保留が空になると backlog キューを読み、未消化アイテムがあれば `/backlog を実行してください` で毎ターン stop をブロックしていた（compass charter が陳腐化していれば代わりに `/compass` を促して撤退）。同じ要求を **SessionStart** フックが `バックログに N 件 (最優先: '…')。/flow で開始しますか？` としてセッション冒頭でも出していた。**この3経路（SessionStart 提案 / Stop の backlog アーム / その compass ゲート）はユーザーの指示で撤去した**。キューを消化するかどうかは操作者の判断であり、`/flow` や `/backlog` を明示的に起動したときだけ行われる。backlog クレート自身が SessionStart でキューの状態を注入するのは変わらない — 無くなったのは「それを今やれ」という指示だけである。

中身はセッションごとの状態機械で、各フェーズが「ターンをブロックして `/` コマンドで誘導するか」「そのまま終了させるか」を決める。

| フェーズ | 条件 | autoflow の動作 |
|---|---|---|
| **Idle** | このセッションで十分なターン数とツールイベントがある | block → `/session-insights:record` |
| **RecordRequested / Continuing** | condukt タスクがまだ保留中 | block → `/condukt`（自動は 4 回まで、5 回目以降はユーザーに確認） |
| **Done** | condukt run に保留タスクなし | ターンの終了を許可（backlog キューは見ない） |

継続は回数ではなく**進捗**で決まる: 保留集合が縮んでいる限りブロックし続け、`stuck_threshold` 回連続で縮まなければ**可視の**エスカレーション（自律モードなら継続を宣言、非自律ならユーザーに確認）に切り替える。黙って撤退はしない。また別の生きたセッションが backlog ロックを保持している間は autoflow は完全に撤退し、稼働中の `/flow` や `/backlog` driver を二重に駆動しない（この撤退は促しではなく譲歩なので残してある）。

**PreCompact** では、このセッションが backlog ロックを保持しており（＝実際に `/flow` ループを駆動中）、ユーザーが opt-out（`resume_flow_on_compact = false`）していなければ、resume マーカーを書き込む — compaction 自体をブロックすることはない。続く **UserPromptSubmit** はそのマーカーを（あれば）消費し、「`/flow` を再開せよ」という指示を一度だけ注入する。マーカーが無い通常のターンでは常に黙る。

サブスクリプションネイティブな設計で、3 つのフックと同梱の Rust バイナリだけで動き、**API キーは不要**、デーモンも不要。Stop フックは理由付きの `block` 判定を出すだけで、自身が作業を実行することはない。状態ファイルが無い場合や stdin が空の場合は exit 0 で抜けるため、ターンが壊されることはない。

## どうして必要か

長いセッションは、record を取り忘れたり、走らせた condukt run の保留タスクを残したまま終わりがちである。これらは「ターンが終わった」というだけの理由で人間にもエージェントにも気付かれず、床に落ちたまま忘れられる。autoflow はセッション終了という決定論的なタイミングに「やり残し検査」を割り込ませ、record → condukt の連鎖を確実に回す。

**「やり残し」に backlog キューを含めない**のは 2026-08-20 の方針転換である。走っている run の保留タスクは*このセッションが始めた仕事*なので放置は事故だが、backlog キューは*まだ始めていない仕事*であり、それを毎ターン催促するのは検出ではなく催促にすぎない。促しの回数は情報量を増やさないので、撤去した。

判断（どう作業するか）は引き続き各スキルと LLM が担い、autoflow が担うのは「終わらせてよいか」のゲートだけである。だからこそ暴走しないよう、自動ブロックには上限があり、上限を超えたらユーザーに判断を委ね、他セッションがロックを握っていれば手を引く。

## どう使うか

プラグインマーケットプレイス経由でインストールすると、同梱の `hooks/hooks.json` が **Stop**・**PreCompact**・**UserPromptSubmit** の 3 フックを `${CLAUDE_PLUGIN_ROOT}/bin/autoflow` に自動配線する（**SessionStart** は 2026-08-20 に撤去）。ほかに設定は要らず、ゲートはデフォルトで有効。しきい値（最小ターン数・最小ツールイベント数・停滞しきい値・resume-flow-on-compact）は config のデフォルト値から来る。

スタンドアロン（cargo）で使う場合:

```sh
cargo install --path .
autoflow stop            # Stop フック: record→condukt の状態機械を実行
autoflow pre-compact     # PreCompact フック: このセッションがロックを保持していれば resume マーカーを書く
autoflow prompt-submit   # UserPromptSubmit フック: マーカーを消費して「/flow を再開」を一度だけ注入
```

`autoflow stop` は stdin でフック JSON を読み、`block` 判定を出力する（または何も出力しない）。`autoflow pre-compact` と `autoflow prompt-submit` は resume マーカーのゲートが成立しない限り黙る。`AUTOFLOW_DISABLE=1` でゲートを無効化できる。

同梱の `bin/autoflow-*` バイナリがプラグインの出荷物であり、エンドユーザーは cargo も API キーも不要。フックが依存する挙動を変えたときは、ワークスペースをビルド（`cargo build --workspace --release`）して再コミットする。テストは `cargo test` で実行する。
