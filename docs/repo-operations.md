# リポジトリ運用手順（plugin rollout / version bump / レビュー窓口）

> **このファイルは特定の作業（プラグイン改修・バージョン管理・レビュー統合・特定ゲートスクリプトの
> 詳細）をするときだけ読む。** 全 agent が毎ターン読むべき汎用ドクトリン（fail-open/fail-closed の
> 哲学・判断と観測の関係・worktree 分離義務など）は [`CLAUDE.md`](../CLAUDE.md) の
> 「最上位の方針」節を参照。ここに書かれているのは「どうやるか」の手順であり、「なぜそうするか」の
> ドクトリンではない。

## ゲートスクリプトの詳細（`cargo fmt`/`clippy` 以外）

- **prompt-injection 防御ゲート（1本）** — prompt に load される資産に改竄が植わっていないか機械検出する:
  - `python3 scripts/check-prompt-injection.py`（injectguard）— skills/agents/hooks/CLAUDE.md/.compass/docs
    に隠蔽・検証バイパス・egress 文言が植わっていないか走査（防御 framing は除外）。ローカルは
    `git config core.hooksPath .githooks` で pre-commit を有効化（速い advisory 層。CI の `injectguard` job が
    非バイパスの本ゲート）。
- **Continuous-Audit 自動起動導線** — `git config core.hooksPath .githooks` を有効化していれば
  `.githooks/pre-push` が GATE_CRATES（blastguard/propguard/specguard/stuckguard/mutategate/overwatch）配下の
  変更を検知し、`scripts/continuous-audit.sh --dry-run` を勧める advisory メッセージを出す
  （pre-commit と同じ fail-soft 設計。push は絶対に止めない・常に exit 0）。cron 定期実行の雛形は
  `scripts/continuous-audit.cron.example` を参照。

## 記録そのものの定期監査 — `scripts/record-audit.py`（backlog 0f55003a）

上のゲート群はすべて**コード**を検査する。**それらのゲートが書き出す記録**を検査する層は無かった。
記録は次の監査の入力なので、腐ると次の監査は「役に立たない」ではなく「成立しない」— 書き込みが
止まった台帳から `clean` を結論することになる。

`record-audit.py` は 4 つの記録健全性を測り、閾値超過を `overwatch review-queue` に上げる:

| 次元 | 何を見るか | 閾値（測定 2026-07-31 / `c3f29681`） |
|---|---|---|
| `doc-claim-drift` | docs/ と CLAUDE.md の `path:line` 主張のうち実体と食い違う数 | **ratchet** 140（増加で breach） |
| `audit-convergence` | `overwatch audit-metrics` の tri-state `converging` | false で breach |
| `review-queue-depth` | 人間レビュー面に溜まった未処理エントリ数 | 20 |
| `stale-undisposed` | fix は landed したが disposition が無い finding 数 | 5 |
| `backlog-rot` | 21日超 pending ＋ 再浮上して再 fail した項目 | 0 |

**なぜこの 4 つか**（どれも実測。仮定ではない）: `check-doc-claims.py` は 140 件のドリフト主張を
抱えたまま verdict `clean` を返す（全件 exempt で、exempt が clean として描画される）。
`audit-metrics` は 6 ラウンド連続で `converging: NO`（new findings 5→1→13→0→14→15）だが誰も読まない。
`review-metrics` は stale 件数を println して exit 0 する（`disposition_cli.rs`）。`backlog fail` は
項目を退役させず 2 日 defer するだけで、`Task::is_pending` が再び数える（`task.rs`）。

**exit code は三値**: `0` = 全次元を測定して閾値内 / `1` = 閾値超過（finding を記録済み）/
`2` = **測定できなかった次元がある**。2 が 1 に優先する — 測れなかった次元は通った次元ではない
（CLAUDE.md 第3節）。probe の非0終了・parse 不能・binary 不在・timeout はすべて `2` であって `0` ではない。

**定期実行は `daily`（ローカル）** — CI schedule は使わない。ゲートを止める権限だけでなく
**breach の可視性まで外部サービスに預ける**ことになるためで、advisory であっても割に合わない
（CLAUDE.md 第7節）。`~/.daily/config.toml` に登録済み。雛形は:

```sh
python3 scripts/record-audit.py --print-daily-task   # 貼り付ける stanza を出力
python3 scripts/record-audit.py --no-escalate        # 手で1回だけ測る（queue に何も書かない）
```

> **`~/.daily/config.toml` を新規作成するときの罠**: `daily` はタスクが 1 つも登録されていない間だけ
> 組み込みの `security`（cargo-deny）タスクへフォールバックする（`crates/daily/src/main.rs` の
> `effective_tasks`）。record-audit だけを書いた config を作ると **cargo-deny 監査が黙って退役する**。
> 現行 config は security を明示的に引き継いでいる。

**記録は 2 箇所に残る**: breach は `overwatch review-queue`（`record-audit:<次元>` という finding id。
既に open なら重複記録しない — `record-finding` は単純 append なので毎日積むと面それ自体が読めなくなる）、
全実行の数値は `~/.record-audit/observations.jsonl`（append-only の trend 台帳。
`RECORD_AUDIT_STATE_DIR` で差し替え可）。**台帳への追記に失敗した実行は exit 2** — 記録を残さなかった
実行は、この job が生む唯一の成果物である trend に穴を空けている。

**store は cwd で解決される（重要）**: `overwatch` も `backlog` も store を cwd から引くので、
linked worktree から走らせると**全次元が 0 を返す**（存在しない store を「健全」と報告する）。
`record_audit.record_root()` が `git rev-parse --git-common-dir` で main worktree を解決してそこを読む。
解決できなければ REPO へフォールバックせず **undetermined** にする。テストは
`scripts/test_record_audit.py`（45 ケース。13 の fail-open 変異がそれぞれ名前の付いたテストで殺されることを確認済み）。

## プラグインを改修したときの反映（忘れやすい）

`crates/<name>/` が唯一の正典。`/plugin install` はここを
`~/.claude/plugins/cache/<owner>/<name>/<version>/` に**プレーンコピー**する（git 外）。稼働中の
ハーネスはキャッシュ側を読むので、**repo をビルドしただけでは何も反映されない**:

### 「plugin 更新（`/plugin update` 相当）」を求められたら — `scripts/rollout-plugins.sh` を使う（正典・一発）

**version を上げた／テキスト資産を変えたプラグインを稼働ハーネスへ反映するときは、手動 `cp` ではなく
必ず `scripts/rollout-plugins.sh` を実行する。** このスクリプトは `/plugin update`（UI 専用操作）が
directory marketplace `yukineko` に対してやる2操作 —(1) `crates/<name>/` を新しい
`cache/yukineko/<name>/<version>/` **dir へコピー**、(2) `installed_plugins.json` の
`<name>@yukineko` を新 dir へ **repoint** — を再現し、続けて `rebuild-plugins.sh`（バイナリ swap）と
各 plugin の `sync-plugin-assets.sh`（skills/agents/hooks 同期）まで一括で走らせる。順序も保証する
（version dir + registry pointer を rebuild より先に作る）。

```sh
python3 scripts/check-plugin-versions.py && python3 scripts/check-version-bumped.py   # 先に version 整合を確認
scripts/rollout-plugins.sh --plugin <name> --dry-run   # 動作を確認（何も書かない）
scripts/rollout-plugins.sh --plugin <name>             # 実反映（全 plugin なら無引数）

# GATE クレート（防御ゲート: blastguard / propguard / specguard / stuckguard / overwatch、
# 非プラグインの mutategate も含む）は fleet を守るため、canary 無しの反映を
# **拒否**する（Problem-2.3）。GATE クレートを対象にするときは --canary で段階
# 反映し、各ステージ間で健全性ゲート（raw-spike または systemic recurrence の
# いずれかで rollback）を通す。--plugin 無しの全 plugin 反映も GATE を含むため
# --canary が必須（明示的に外すときのみ --no-canary）。
scripts/rollout-plugins.sh --plugin specguard --canary --dry-run   # GATE クレート: 段階反映を確認
scripts/rollout-plugins.sh --plugin specguard --canary             # GATE クレート: canary 経由で反映
```

- 冪等（version 不変なら no-op）。`--force` で無変更でも再コピー、`--no-rebuild`/`--no-sync` で段階分け。
- **GATE クレートは canary 必須**: canary 無しでの反映はエラーで止まる（fleet 防御ゲートを
  無検証で入れ替えないための安全策）。`--no-canary` は明示的な例外指定としてのみ用いる。
- **手動 `cp` は禁止**: cache の binary/asset を手で上書きするだけだと **version dir が旧名のまま残り**
  registry も更新されず、`sync-plugin-assets.sh` が version から dir を誤解決して**古い版が配布される**。
- `/plugin update`（ユーザー UI 操作）は本スクリプトで完全代替できるので、手動 UI 操作は不要。

低レベルの構成要素（通常は上の rollout 経由で十分。個別に叩くのは段階分けのときだけ）:

- バイナリを反映: `scripts/rebuild-plugins.sh`（`--no-clean` で増分）— target のバイナリを live
  キャッシュへ swap する（**既存 dir に swap するだけ。version dir は作らない**）。
- テキスト資産（skills/agents/hooks）を反映: `crates/<name>/scripts/sync-plugin-assets.sh`
  （`--check` で drift 検出）。
- **キャッシュを手編集しない**（git 外で黙って乖離する）。必ず repo を編集 → 上記で同期。

## バージョン整合（**絶対厳守** — 「今動けばいい」は後で壊れる）

各プラグインの version は **3つの正典で常に lockstep** でなければならない。片方だけ上げた状態は
**バグ**として扱う（「今は動く」で放置しない）。ズレると sync-plugin-assets.sh が version から
キャッシュ dir を誤解決し、**古い版がユーザーに配布される**。

### 変更したら必ず version を上げる（**禁忌ルール**）

- **あるプラグインの中身（コード・hooks・skills・agents 等いずれか）を1行でも触ったら、その
  プラグインの version を最低でも micro（patch, `x.y.Z` の Z）上げる。** 触ったのに据え置きは
  **禁忌**。
- version を上げるときは **必ず3ファイル同時**: `crates/<name>/Cargo.toml` の `[package].version`
  ／ `crates/<name>/.claude-plugin/plugin.json` の `version` ／ `.claude-plugin/marketplace.json`
  の当該エントリ `version`（skill-only プラグインは Cargo.toml が無いので後者2つ）。
- 言い換え: **「何か変更したのに plugin version と marketplace version が変わっていない」状態を
  commit してはならない。** 変更の大小を問わず、最低 micro を必ず上げる（意味的に大きければ
  minor/major で判断）。
- これは「今動けばいい」を禁じ、後の drift・古い版配布を根絶するための**徹底順守ルール**。

**強制ゲート（3つとも、commit 前・push 前・CI で回す）:**

```sh
python3 scripts/check-plugin-versions.py            # lockstep: 3ファイルの version が一致するか（exit 1 で drift）
python3 scripts/check-version-bumped.py             # bump-on-change: base(既定 HEAD)から変更のある plugin が bump 済みか
python3 scripts/check-version-bumped.py --base origin/main   # CI/push前は pushed ref と比較
python3 scripts/check-plugin-rollout.py             # rollout drift: source version が installed_plugins.json の registry version へ実際に反映(rollout-plugins.sh実行)済みか
```

`check-plugin-rollout.py` は上の2つとは別の失敗モードを塞ぐ: **source 3ファイルの version 整合と
bump-on-change を両方満たしていても、`rollout-plugins.sh` を実際に実行し忘れれば、その fix はどのセッションにも
一切反映されない。** 過去に一度、5個の plugin（hypothesis/condukt/compass/blastguard/overwatch）で
commit・version-bump 済みの fix が rollout されないまま放置されていたことが手作業の grep+jq 調査で発覚した。
このスクリプトはその調査をスクリプト化したもので、`installed_plugins.json` の `"<name>@yukineko"` registry
version と各 plugin.json の source version を比較し、不一致（＝ rollout 未実行）を機械的に検知する
（registry ファイルが存在しない環境では検査対象なしとして exit 0 で skip）。

`check-version-bumped.py` は `crates/<name>/` に差分がある plugin の plugin.json version が
base より**厳密に上がっている**ことを要求し、上がっていなければ exit 1 で該当 plugin と変更ファイルを
表示する（新規 plugin は base に無いので OK）。**「変更したのに未 bump」を機械的に止めるゲート**。

**共有クレート（`crates/harness-core`）にも同じ規則がかかる**（backlog `32170548`）。harness-core は
plugin ではないので上のループの対象外だったが、**36 plugin のバイナリに静的にリンクされる**ため、
harness-core が動いた時点で出荷済みバイナリ 36 本の中身が変わる — にもかかわらず plugin.json は
どれも動かない。「バイトが変わったのに version が動かない」＝ version が出荷物を同定しない状態なので、
**harness-core 自身の `[package].version` の bump を要求する**（`crates/harness-core/Cargo.toml` のみ。
harness-core は plugin.json も marketplace.json も持たないので lockstep 3ファイルの対象ではない）。
- **リンクされる差分だけ**が bump を要求する。`crates/harness-core/tests/`（独立した integration
  test target）と `*.md` は構造的にバイナリへ入らないので免除され、**その免除は必ず標準出力に announce
  される**（黙って narrow しない）。それ以外は — `src/` 内の `#[cfg(test)]` ブロックのように
  parse なしには判別できないものも含めて — リンク扱いで bump を要求する（§3 の「判定不能は制限側」）。
- **リンク先 36 plugin の bump は要求しない。** 意味論的には最も純粋な読みだが、harness-core 1 commit
  あたり 108 ファイル + marketplace.json 36 行が動き、並行セッションと必ず衝突する（§8）。バイト同定は
  それ無しでも機械的に取れる（下記 `harness_core_version` と `SHARED_SOURCE_PATHS`）。
- テスト: `scripts/tests/version-bumped-shared-crate.sh`（7 ケース。未 bump が落ちること・tests/ 免除が
  announce されること・version が読めない場合に fail-closed に倒れることを含む）。

`rebuild-plugins.sh` が書く `.deployed-from.json` には **`harness_core_version`** が入る（そのバイナリが
リンクした harness-core の version）。`check-plugin-rollout.py` はこれを source tree の値と比較し、
遅れていれば rollout drift として落とす。読めなかった場合に書かれる `"unknown"` も **問題として扱う**
（もっともらしい version を捏造しない）。フィールドが**無い**旧 manifest は問題にしない — 同じ問いは
`SHARED_SOURCE_PATHS`（`crates/harness-core` を含む）による commit 比較が既に完全に答えるので、
fallback は弱くない（この「弱くない」は仮定ではなく
`scripts/test_check_plugin_rollout.py::SharedCrateVersion` の両アームで観測してある）。

- 正典3ファイル（バイナリ付きプラグイン）: `crates/<name>/Cargo.toml` の `[package].version` /
  `crates/<name>/.claude-plugin/plugin.json` の `version` / `.claude-plugin/marketplace.json` の
  当該エントリ `version`。**skill-only プラグイン**（Cargo.toml 無し。例: `scout`）は
  plugin.json + marketplace.json の2つ。
- 正典の向き: **`Cargo.toml == plugin.json` が真**。`marketplace.json` は取り残されやすい（laggard）。
  version を上げるときは **必ず3ファイル同時**に上げる。marketplace.json だけ手で上げ忘れない。
- **すべてのタイミングで徹底チェック**（version を触る commit、rebuild 前、push 前）。
  自動チェッカで機械的に確認する:

  ```sh
  python3 scripts/check-plugin-versions.py   # exit 0 = 全整合 / exit 1 = drift（該当プラグインを表示）
  ```

- **rebuild は version を上げない**（別工程）。`rebuild-plugins.sh` は正典の version をそのまま
  コンパイルして live cache へ swap するだけ。version bump は 3ファイル編集という別の意図的操作。
- **cache の version dir が古い**（例: source は 0.7.0 だが `cache/.../<plugin>/0.6.0/` のまま）のは
  `/plugin update`（ユーザー UI 操作）未実行が原因。rebuild-plugins.sh は既存 dir にバイナリを
  swap するのでコードは動くが、正式ロールアウトは `/plugin update` → rebuild → sync-plugin-assets.sh
  の順。

## 人間のレビュー窓口（統合レビューサーフェス）

観測系の3ストリーム — (1) systemic な gate 違反（`overwatch violations --systemic`）、
(2) canary health-gate のロールバック事象、(3) AI/敵対的レビューの指摘 — は従来別々だった。
`overwatch review-queue` はこれらを **1本の時系列リスト**（新しい順）にまとめ、各行を
`[systemic]` / `[rollback]` / `[ai-finding]` のタグで区別して見せる:

```sh
overwatch review-queue                 # 人間可読の統合リスト（新しい順）
overwatch review-queue --json          # kind 判別子付きの構造化配列
overwatch review-queue --since <ts> --limit <n>
overwatch review-queue --to-backlog    # キュー全体を backlog に流し込む（discover→fix を閉じる）
```

（`[escalation]` = condukt の blocked/GATED タスクが人間の回答待ちで滞留している事象も4本目のストリームとして統合表示される。）

- **fail-soft**: いずれかのソースが空/欠落でも他のソースは表示される（コマンド全体はエラーにしない）。
  **review-queue は verdict を返さない純粋な観測・描画レイヤ**なので、欠落ソースは「違反なし」ではなく
  「そのソースは表示されない」として縮退する（判定を持たないから許される。冒頭「最上位の方針」を参照）。
- ロールバック事象は `scripts/rollout-plugins.sh` の canary auto-rollback 時に
  `overwatch record-rollback` で追記される（fail-soft: 記録失敗はロールアウトを止めない）。
- AI 指摘の永続ストア（`overwatch record-finding` の取り込み口）は Continuous-Audit ループ（別 backlog）が
  埋めるまで通常は空で、その場合この行は出ない（graceful degrade）。
- **`--to-backlog`（review-queue → backlog consolidation）**: レンダリングの代わりにキュー**全体**
  （ai-finding / systemic / rollback / escalation の4種）を1件ずつ `backlog add` へ転送し、`/flow` が
  自動修復できるようにする。冪等 — findings は `bridged_findings.jsonl`（bare finding-id。review-metrics の
  「解消済み」判定源でもある）、他3ストリームは `bridged_entries.jsonl`（`<kind>:<identifier>`）で二重投入を防ぐ。
  fail-soft: store 欠落 / backlog バイナリ不在 / `backlog add` 失敗はいずれも warn してスキップし、コマンドは常に成功する。
