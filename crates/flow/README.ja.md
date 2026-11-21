# flow

```
SOURCE（課題の供給）                          EXECUTOR（解決手段の実行）
  compass     … 次の右サイズの一手             ─┐
  backlog     … 確定済みキュー（+ GitHub issues mirror）├─▶  condukt（fugu-router がモデル選択）─▶ verify
  hypothesis  … PDO 仮説の build / measure      │
  prompt      … ユーザー直の課題文             ─┘
```

## 止まらない契約（ユーザー裁定 2026-11-21）

裁定の逐語は `skills/flow/SKILL.md` の「止まらないこと」節を正とする。要約:

- **引数なし**: 各サイクルの先頭で `backlog sync --apply`（store と GitHub issues mirror の照合）を行い、
  `backlog next --claim` でピックする。キュー（backlog の pending・計測できる仮説・compass の一手）が一手も出さなくなるまで回す。
- **引数あり**: その課題を claim し（課題文から決めた hashkey で `condukt state claim-task --stateless`）、
  解決して検証（condukt の完了ゲート＋検証）を通るまで進める。引数が「backlog を消化せよ」型なら backlog が空になるまで回す。
- **唯一の停止理由は `condukt circuit check` の trip**（常に `--no-compaction-axis` 付きで呼ぶ）。
  予算の逼迫・超過・判定不能はバッチ幅を N=1 にするだけ。compaction は停止理由ではない。人間の判断待ちは撤去した。
- **human gate**: 自律モードでは各ゲートを `condukt policy answer --resolve` に通し、
  **auto**（exit 0・推奨の `chosen` を採用）/ **deny**（exit 3・破壊的なのでその操作を拒否）/
  **reject**（exit 4・未測定なので完了と認めない）/ **cancel**（exit 5・解決できないので別の問いで 1 度問い直し、だめなら item を cancel/fail）
  のどれかに必ず解決する。どれもループを終わらせない。非自律では従来どおり `AskUserQuestion`。

キュー全体は**ロックしない**: 同じ project で複数セッションが同時に `/flow` を回してよい。
排他は task 単位（選択と予約を同一クリティカルセクションで行う `backlog next --claim` と `condukt state claim-task`）で、各セッションは `backlog driver register` で非排他に driver 登録する。

手順の正典は `skills/flow/SKILL.md`:

```
0. 引数分岐 — 課題文があれば claim して、その課題が解決・検証を通るまで condukt で進める
     （「backlog を消化せよ」型の引数は通常ループで backlog が空になるまで）
0.5. 自律ゲート — `condukt state autonomy-check`。自律モードなら各 human gate を
     `condukt policy answer --resolve`（auto / deny / reject / cancel）に通す
1. compass は先に引かない — backlog 優先。compass は backlog が一手も出さなかったときのフォールバック
2. driver 登録（非排他）— backlog driver register。キュー全体はロックしない
2.5. flow-watch を Monitor で武装（停滞検知）
3. 実行ループ — backlog sync --apply → budgetguard（情報。バッチ幅だけ）→ ピック（backlog next --claim）
       → spec-gap ゲート → claim → overwatch begin → /condukt → 検証 → sink（done / fail）→ release
       → heartbeat → condukt circuit check --run "flow-$CLAUDE_CODE_SESSION_ID" --session "$CLAUDE_CODE_SESSION_ID" --no-compaction-axis（trip だけが停止）
4. 終了 — キューが空 / 引数の課題が解決 / circuit trip で backlog driver unregister
     + overwatch end + サマリ報告 + pivot-check（--resolve、推奨＝継続）
```

## 構成 — Stop hook を持つ Rust クレート

flow はもう skill のみのプラグインではない。Stop hook 用のバイナリを持つ workspace クレートである:

```
.claude-plugin/plugin.json     # プラグインマニフェスト
Cargo.toml / src/              # `flow` バイナリ（src/stop_gate.rs・decide.rs・probes.rs）
bin/flow                       # POSIX ランチャ → bin/flow-<os>-<arch>
bin/flow-watch                 # Monitor で常駐させる停滞検知（Step 2.5）
hooks/hooks.json               # Stop → ${CLAUDE_PLUGIN_ROOT}/bin/flow stop-gate
skills/flow/SKILL.md           # /flow skill（source→executor ループを駆動）
```

### `flow stop-gate`（Stop hook）

- **live な登録済み driver** のセッションは、`backlog next` が仕事を返す、または `flow-<SESSION>` の claim を保持している間、
  stop を block する。ただし `condukt circuit check --run flow-<SESSION> --session <SESSION> --no-compaction-axis` が trip していれば許す。
- 判定不能（registry・backlog・claims・circuit を観測できない）はまず 1 回 block し、同じセッションで連続した次の
  判定不能の stop だけを、理由を表示したうえで allow にする。gate 本体の panic は block（fail-closed、`harness_core::gate::run::run_guarded`）。
- driver 登録していないセッションは**一切** block しない（`/flow` を提案する hook ではない）。
- Step 4 の `backlog driver unregister` で終わる。skill が unregister するのは、キューが空になったとき・circuit が trip したとき・引数の課題が解決したときだけ。

**flow は今も自分から提案しない。** 0.2.6 までは `flow propose` という SessionStart hook が L2 の
propose-then-confirm ディレクティブを注入していたが、ユーザーの指示で廃止した（2026-08-20。autoflow 側の同種の 2 経路も同時に撤去）。
上の Stop hook は `/flow` を起動して driver 登録したセッションにしか作用しない。

```text
/plugin marketplace add yukineko/claude-harnesses
/plugin install flow@yukineko
```

## テスト

```sh
cargo test -p flow --test stop_gate        # Stop hook の挙動
cargo test -p flow --test skill_contract   # SKILL / README の文面契約（2026-11-21 裁定）
cargo test -p integration-tests --test flow_skill_queue_contract   # キュー全体ロック無し・backlog list でピックしない
```

MIT
