セッション終了時の auto-flow ゲート — `/record` を促した後は、走っている condukt run に保留タスクがある間セッションが終わるのを防ぐ **Stop** フックと、`/compact` を跨いでもループを継続させる **PreCompact** + **UserPromptSubmit** の組。

具体的には、ターン完了時にまず一度だけ `/record`（`/session-insights:record`）を促し、続いて**現在の condukt run** の保留タスクが片付くまで `/condukt` をループで促す。
run-state を読めなかった場合は黙らずにブロックする。

<!-- record: 2026-08-20 rev: 5289312f -->
backlog キューは 2026-08-20 に対象外になった。それ以前は、condukt の保留が空になると backlog キューを読み、未消化アイテムがあれば `/backlog を実行してください` で毎ターン stop をブロックしていた（compass charter が陳腐化していれば代わりに `/compass` を促して撤退）。同じ要求を **SessionStart** フックが `バックログに N 件 (最優先: '…')。/flow で開始しますか？` としてセッション冒頭でも出していた。この3経路（SessionStart 提案 / Stop の backlog アーム / その compass ゲート）はユーザーの指示で撤去した。
<!-- /record -->

backlog キューは対象外であり、未消化の backlog アイテムがあっても Stop はブロックされない。

- **Idle**（このセッションで十分なターン数とツールイベントがある）: block → `/session-insights:record`。
- **RecordRequested / Continuing**（condukt タスクがまだ保留中）: block → `/condukt`。保留集合が縮んでいる限り継続し、回数上限は無い。
- **RecordRequested / Continuing**（保留集合が `stuck_threshold` 回連続で縮まなかった）: 可視のエスカレーション。
- `stuck_threshold` の既定値は 3 である。
- **RecordRequested / Continuing**（condukt の run-state を読み取れない）: 「判定不能」の理由付きで block する。フェーズは更新しないので次の Stop で観測し直す。
- **Done**（condukt run に保留タスクなし）: ターンの終了を許可し、backlog キューは見ない。

停滞時に黙って撤退はしない。
エスカレーションは共有の autonomy スイッチ（`harness_core::autonomy`）から自律かどうかを読み、自律なら継続を宣言、非自律ならユーザーに確認する文言でブロックする（どちらもブロックする）。
エスカレーションする Stop だけは保留タスクを `running` に再マークしない。

- **ユーザーの明示的な停止**（状態機械より前）: transcript 上でユーザーの直近のシグナル（入力したメッセージ、または `AskUserQuestion` への回答）が明示的な停止指示（「止めて」「stop」等）なら、*その* Stop は何もせず通す。
- 明示的な停止は `Done` には latch しないので、ユーザーが作業を続ければ促しは再開する。
- 読めない・無いシグナルを停止とみなすことはない。
- **委譲記録の advisory**（RecordRequested / Continuing 中）: このセッションの transcript で `/flow` が condukt run を完了させたのに `fugu-router record` が呼ばれていなければ、セッションにつき一度だけ `fugu-router record --class flow-delegation …` を促してブロックする。

`backlog lock status` がこのプロジェクトの生きた driver を報告している間は autoflow は完全に撤退し（Stop を許可し）、稼働中の `/flow` や `/backlog` driver を二重に駆動しない。
backlog が*観測上*インストールされていない場合は撤退しない。
撤退するのは生きた driver が*積極的に*報告された場合だけである。
driver の稼働状況を判定できない場合（`backlog lock status` が非0で終了した・空または解釈不能な出力をした・自身の `undetermined` オブジェクトを返した、または plugin cache が読めない）、Stop は「判定不能」の理由付きで **block** し、フェーズは更新しない。
session id が無い Stop（payload にも `CLAUDE_CODE_SESSION_ID` にも無い）も同様に block する。
無限ブロックを避けるため、これらの判定不能 block は `stop_hook_active` で上限が付く。
そうした block の後の再入時は stop を通し、stdout には何も出さない（`systemMessage` も出さない。2026-10-27 のユーザー裁定「Stop時に結果の報告はあってもいいがログはいらない」。理由は最初の block で既に示している）。代わりに `<state_dir>/log.jsonl` へ `{"event":"bounded_allow","session_id","what","why"}` の行を追記し、clean pass と区別できる記録を残す。
ユーザーの明示的な停止は、稼働状況が判定不能でも優先される。

**PreCompact** では、このセッションが `backlog lock status` 上で排他ロック保持者または登録済み driver の一つであり（＝実際にこのセッションから `/flow` ループを駆動中）、ユーザーが opt-out（`resume_flow_on_compact = false`）していなければ、resume マーカーを書き込む。
PreCompact は compaction 自体をブロックすることはない。
このセッションが driver かどうかを判定できない場合（session id が無い、または `backlog lock status` が失敗・解釈不能）、PreCompact はマーカーを書かず、その旨を可視の `systemMessage` で出す。
続く **UserPromptSubmit** はそのマーカーを（あれば）消費し、「中断した /flow の loop を次の一手から再開せよ」を含む指示を一度だけ注入する。
マーカーが無い通常のターンでは常に黙る。

Stop フックは理由付きの `block` 判定を出すだけで、作業そのものを実行することはない（副作用は自身のセッション状態ファイル、通常の継続時に保留 condukt タスクを `running` にマークすること、block ごとの overwatch violation 記録のみ）。
ホスト向けバイナリが無い場合も launcher が `stop` に対して `block` を出す。
ゲートが無効化されている場合、condukt の run ファイルが存在しない場合（condukt がこのプロジェクトで走ったことが無い＝「保留タスク無し」の観測）、上記の撤退条件の場合も Stop は黙って終わる。

プラグインマーケットプレイス経由でインストールすると、同梱の `hooks/hooks.json` が **Stop**・**PreCompact**・**UserPromptSubmit** の 3 フックを `${CLAUDE_PLUGIN_ROOT}/bin/autoflow` に自動配線する。
 **SessionStart** フックは撤去された。
しきい値（最小ターン数・最小ツールイベント数・停滞しきい値・resume-flow-on-compact）は config のデフォルト値から来る。ゲートはデフォルトで有効である。
上書きするには `~/.autoflow/config.toml` を書く:

```toml
enabled = true                # 既定 true
min_turns = 2                 # /record を促すまでのターン数
min_tool_events = 3           # /record を促すまでのツールイベント数
stuck_threshold = 3           # 可視エスカレーションまでの無進捗 Stop 回数
resume_flow_on_compact = true
state_dir = "~/.autoflow/state"
```

パースできない config ファイルは無視される（既定値が使われる）。

```sh
cargo install --path .
autoflow stop            # Stop フック: record→condukt の状態機械を実行
autoflow pre-compact     # PreCompact フック: このセッションがロックを保持していれば resume マーカーを書く
autoflow prompt-submit   # UserPromptSubmit フック: マーカーを消費して「中断した /flow の loop を次の一手から再開せよ」を含む指示を一度だけ注入
```

`autoflow stop` は stdin でフック JSON を読み、`block` 判定を出力する（または何も出力しない）。
`autoflow pre-compact` と `autoflow prompt-submit` は resume マーカーのゲートが成立しない限り黙る。
例外として、`pre-compact` はゲートを判定できなかった場合に可視の `systemMessage` を出す。
`AUTOFLOW_DISABLE=1` でゲートを無効化できる。

```sh
cargo test
```

コミットされているのは `bin/autoflow` launcher だけである。
これがプラットフォーム別の `bin/autoflow-<os>-<arch>` を exec する。
後者は gitignore されている。
フックが依存する挙動を変えたときは rollout をやり直すこと。
