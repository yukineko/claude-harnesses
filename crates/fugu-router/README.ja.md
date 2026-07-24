# fugu-router

Claude Code オーケストレーション向けの fugu 風モデルルーティング。タスクごとにどの Claude ティアを走らせるかを、過去の実績から決定論的に決める。

## 目的

fugu-router は、condukt が生成したタスク分解 JSON を受け取り、各タスクの `suggested_model` を「実績から学習した方策」で上書きするルーティング層である。

[Sakana AI の fugu](https://sakana.ai/fugu-release/) は、**訓練済みのコーディネータ**がリクエストを役割（Thinker / Worker / Verifier）ごとに専用モデル群へ割り当て、その成果を検証・統合する。このコーディネータは進化戦略や強化学習で学習される。Claude の重みは学習できないので、fugu-router は fugu の**形（コーディネータという独立した決定論コンポーネント）**だけを残し、*学習された判断*を**実績の検索**に置き換える。

```
record で実績を蓄積 ── episodes.jsonl ──▶ 似た過去タスクを k-NN で検索
(model, pass?, cost)                       │
                                           ▼
                            しきい値を歴史的に満たす
                            最安ティア  →  suggested_model
```

つまり fugu は router を*学習*するが、fugu-router は方策を*検索*する。粒度はトークン単位ではなくタスク単位と粗いが、その代わり API キーも埋め込みサービスも要らない。

学習は二系統を意図的に併用する。

- **非パラメトリック（検索）:** 記録済みエピソードに対する k-NN。1 件の追加で挙動が変わる即時適応、完全に解釈可能、悪いエピソードを消すだけで訂正できる。類似度は語尾のステミングとドメイン概念辞書（`semantic.rs`）で、login ↔ auth ↔ session のような同義語を橋渡しする（純粋な字面一致では取りこぼす）。
- **パラメトリック/オンライン（バンディット）:** ティアごとの Beta(passes, fails) 事後分布に対する Thompson サンプリング（`explore` で切替）。各ティアの合格確率を引き、しきい値を満たす最安ティアを選ぶため、実績の乏しい安価なティアも不確実性下で*試され*、結果から方策が更新される。報酬（どのティアが見合うか）を実際にオンライン学習する。

限界は率直に述べる。ルーティングはターン単位ではなくタスク単位であり、隠れ状態ヘッドによるルーティングもニューラルな重み更新も無い。バンディットが学ぶのは報酬であって深い表現ではない。意味的な橋渡しも埋め込みではなく辞書ベースである。

## fugu との対応

| fugu | fugu-router |
|---|---|
| 訓練済みコーディネータ | エピソードストアに対する決定論的方策 |
| エージェントプール / ティア | Claude のティア `haiku < sonnet < opus` |
| 役割割り当て | タスクごとの `worker_model` + 独立した `verifier_model` |
| 「いつ委譲するか / どのモデルか」 | しきい値を満たす類似タスクの中で最安のティア |
| 学習 (CMA-ES / RL) | 記録済み実績に対する検索 + **オンラインバンディット**（Thompson サンプリング） |

## どうして必要か

condukt はオーケストレーションの背骨だが、「どのタスクにどのモデルを充てるか」を毎回 interpreter の直感に委ねると、次の痛みが残る。

- **モデル選択が経験から学ばない。** interpreter の `suggested_model` はその場のキーワード判断で、過去に同種のタスクを安いティアが通したという**実績が反映されない**。fugu-router は検証を通った実績を蓄積し、似た過去タスクで合格率がしきい値を超えた最安ティアを選ぶので、毎回賢くなる。
- **コスト過多か品質不足のどちらかに振れる。** 安全側に倒して常に opus を使えば高コスト、安易に haiku を使えば検証落ちが増える。fugu-router は pass 率としきい値（`pass_threshold`）に基づき、歴史的に基準を満たす**最安ティア**を選んでこのトレードオフを定量化する。
- **検証バイアスの自己強化。** `record` の合否は verifier が兄弟タスクの成果を自己判定した結果なので、偏った verifier が悪いルーティングを強化しうる。`label` で人間が訂正でき、人間の判定は方策集計で `pass` を上書きする（`Episode::effective_pass`）。
- **実績がマシンに閉じる。** 1 台で貯めた実績が他マシンに伝わらないと学習がやり直しになる。stores を git 管理し `import` でマージできるので、知見をマシン間で共有できる。

判断（解釈・実装・検証）は LLM、ルーティング（実績検索・ティア選択）は決定論バイナリ、と割り切ることで、再現性とコスト効率を保ちつつ condukt の判断を強化する。結合は**ソフト**で、`fugu-router` バイナリが無ければ condukt は interpreter 自身の `suggested_model` にフォールバックし、何も壊れない。

## どう使うか

### condukt と併用する（主用途）

スキル `/fugu-router [decomp.json | 課題文]` で起動する。condukt の Phase 2（`validate` と `schedule` の間）で `route` を噛ませ、`suggested_model` を学習方策で上書きする。

```bash
condukt validate --file decomp.json
fugu-router route --file decomp.json --report /tmp/route.json > decomp.routed.json
condukt schedule --file decomp.routed.json
```

stdout は `suggested_model` を更新した分解 JSON（そのまま condukt へ渡す）。`--report` ファイルには condukt のスキーマに無い助言——とりわけ **`verifier_model`**（独立した、通常は別ティアの検証モデル）——がタスク id ごとに入る。worker は `suggested_model`、verifier は report の `verifier_model` で起動する。

### 結果を書き戻す（学習信号）

各タスクの検証後、結果を `record` で蓄えると次回が賢くなる。

```bash
fugu-router record --title "<task title>" --files "<touched_files>" \
  --class parallel --model sonnet --status verified --cost 0.09
```

`--status` が合格語（`verified|pass|passed|ok|true`）以外なら非合格として数える。`--cost` は任意（gauge から読めばコスト考慮ルーティングになる）。`--files` に絶対パスを渡すと記録時にリポジトリ相対パスへ正規化され（`/Users/yuki/src/harness/crates/x.rs` → `crates/x.rs`）、マシン固有のパスがストアに混入しないので k-NN の精度が落ちない。`--skill-fingerprint "$(fugu-router fingerprint)"` を渡すと、その実績を生んだ SKILL.md コーパスのバージョンで刻印できる。

`--duration <秒>`（任意、既定 `0.0`）はタスクの実測 wall-clock 所要時間を記録する（計測目的のみで routing/scoring からは一切参照されない）。

`--delegation <fork|inline>`（任意、既定は未設定）は、そのエピソードがどちらの subagent delegation 戦略で生成されたかを記録する。fork/inline のコスト・所要時間比較用（リポジトリ直下の `docs/design-delegation-strategy-measurement.md` 参照）。`--class` と同様バリデーション無しの自由記述で、この比較と無関係な通常の worker/verifier 記録では省略してよい。`route`/`decide_bandit` から参照されることはない。

**condukt と併用する場合、`record` の発火は手書き不要で、condukt の Stop hook が `condukt state record-run --all` で決定論的・冪等に行う**（手書き snippet は単発／condukt 非併用時のフォールバック）。

### モード軸（fast / normal / high）

`route`/`suggest` は `--mode fast|normal|high` を受け付ける。方策（`decide`/`decide_bandit`）が決めた `Decision` に対する**後段の決定論的クランプ**であり、学習ロジック自体は一切変更しない。

- `normal` — 恒等（互換のための既定値）。方策の選択をそのまま使う。
- `high` — worker を1ティア上げる（`opus` で頭打ち）。
- `fast` — worker を1ティア下げる（`sonnet` で頭打ち＝`opus` は worker にも verifier にも絶対に選ばれない）。

`fast`/`high` いずれも、verifier は**クランプ後の worker から** `policy::verifier_model` で再計算する（元の verifier をそのまま引き継がない）。`fast` の場合はさらに verifier を `sonnet` に上限クランプする。

優先順位: `--mode` フラグ ＞ 環境変数 `FUGU_ROUTER_MODE` ＞ `config.toml` の `mode` ＞ 既定 `normal`。**不正な値（env・config いずれも）は `normal` へ黙って倒れず、明示エラー＋非0終了になる。**

`class=gated`（人間承認が必要な opus/opus）はどのモードでも**無変化**（`downgrade_for_budget` の gated no-op と同じ扱い）。

**適用順序: モードのクランプが先、`downgrade_for_budget`（予算逼迫時の値下げ）が後。** 予算はハードな資源制約、モードは選好にすぎないので、予算が最終的に勝つ。`mode=high` が予算逼迫で打ち消された場合も、rationale にはモードの適用結果と予算による打ち消しの両方が残る（`downgrade_for_budget` は既存の rationale に追記するだけで上書きしないため、`high` が静かに消えることはない）。

`record --mode <fast|normal|high>`（任意）でそのエピソードが走ったモードを記録できる。省略時は「未記録」であり、`normal` だったことには**しない**（別々の事実として区別する）。

### その他のサブコマンド

```
fugu-router suggest --files src/auth/login.ts "fix login validation"  # 単発でモデルの当たりを見る
fugu-router confidence --files src/auth/login.ts "fix login validation"  # [0,1] の較正済み合格確率
fugu-router procedures search --query "fix login validation" --files a,b --k 3
                                             # 似た検証済みタスクの解き方を k-NN で引く
fugu-router stats [--json]                  # モデル別 pass率 / 平均コスト（HOTL 可視化）
fugu-router label "add login" --verdict bad --by human   # 人間が実績を訂正（--latest も可）
fugu-router fingerprint [--dir crates]      # SKILL.md コーパスのバージョンスタンプ
fugu-router import --episodes /path/episodes.jsonl [--playbooks ...] [--dry-run]  # 別マシンの stores をマージ
fugu-router import --dedup                  # ローカル stores の重複除去（content-hash, first-seen 優先）
fugu-router sync [--pull-only | --push-only]  # record ストアを sync_repo（git）と同期
fugu-router init                            # fugu-router.toml を書き出す
```

`procedures search` サブコマンドは、似た検証済みタスクが*どう解かれたか*を k-NN で引いて condukt の interpreter を seed する（独立した `playbook` プラグインの知識ノート注入とは別物。旧名 `playbook` は隠しエイリアスとして残る）。

`confidence` サブコマンドは、`route`/`suggest` と同じ k-NN 近傍の類似度加重 pass率
（`Episode::effective_pass` 経由なので `label` の人間判定も反映される）を `[0,1]` の一値で返す。
近傍が `min_samples` 未満なら中立の prior `0.5` にフォールバックする（履歴不足はエラーではなく
最大不確実な回答として扱う）。純粋関数・決定論（同じストア状態＋同じ入力→バイト同一の出力）で LLM は使わない。

### インストールと配線

プラグイン版はバイナリと UserPromptSubmit フックを同梱する。フックは、プロンプトがコーディング作業に見えるときルーティングメモリの要約を 1 ブロック注入する。API キー不要で**サブスクリプションで完結**する。

手動導入する場合は次のとおり。

```bash
cargo build --release
cp target/release/fugu-router ~/.cargo/bin/
fugu-router init                  # 設定（任意）
fugu-router install --dry-run     # settings.json の変更をプレビュー
fugu-router install               # UserPromptSubmit フックをマージ
```

削除は `fugu-router uninstall`。`FUGU_ROUTER_DISABLED=1` で無効化（no-op）できる。

### 設定

`~/.fugu-router/config.toml`（`fugu-router.example.toml` 参照）。主な調整項目は `pass_threshold`（安いティアを信頼する前にどれだけ確信が要るか）、`min_samples`（コールドスタート prior を抜けるのに必要な履歴量）、`sim_threshold`（過去タスクをどれだけ似ていれば数えるか）。

| キー | 既定値 | 説明 |
|---|---|---|
| `store_file` | `~/.fugu-router/episodes.jsonl` | エピソードストアのパス（git 管理下のパスへ向ければマシン間共有できる） |
| `playbook_file` | `~/.fugu-router/playbooks.jsonl` | Playbook ストアのパス（同上。両ストアは独立に git 管理できる） |
| `sync_repo` | *(未設定)* | `fugu-router sync` 用のリモート git リポジトリ URL。設定すると `store_file`/`playbook_file` は既定で `<sync_dir>/{episodes,playbooks}.jsonl` になる |
| `sync_dir` | `~/.fugu-router/record-repo` | `sync_repo` のローカル clone 先 |

マシン間で stores を共有する方法は 2 通り。**`sync`（管理された git リモート）:** `sync_repo` に git リポジトリ URL を設定すると、`store_file`/`playbook_file` は `<sync_dir>/{episodes,playbooks}.jsonl`（`sync_dir` 既定 `~/.fugu-router/record-repo`）を指す。`fugu-router sync` はまずリモートから pull し、続いてローカルの変更を commit & push する（`--pull-only` / `--push-only` で片側のみ）。**手動（`import`）:** `store_file` と `playbook_file` を自分で管理する git リポジトリ内のパスに向け、`git pull` 後に `import` するだけでマシン間同期が完結する（content-hash で重複排除されるので同じ実績を二度引いても安全）。

### コールドスタート

ストアが空のときはキーワード prior を使うが、**安い方向にバイアス**する。履歴が無い時は floor（`haiku`）から始め、verifier のカスケードエスカレーション（検証落ちで haiku→sonnet→opus）が本当に必要なタスクだけ買い上げる:

- design/refactor/migrate/security キーワード → `opus`（高stakes は優先で opus）
- 非常に広い変更（11ファイル超）→ `opus`、中規模（6〜10ファイル）→ `sonnet`
- rename/format/docs/typo → `haiku`（5ファイル超の広い trivial 一括は `sonnet`）
- それ以外（通常の小規模変更）→ `haiku`

独立 verifier も低stakesでは安くする。`opus` ワーカーは `sonnet` で検証、低stakesの `sonnet` ワーカーは `haiku` で検証、`haiku` ワーカーは独立性のため一段上の `sonnet` で検証する。serial/design は従来どおり `opus` verifier。`gated` タスクは自動ルーティングしない（人間承認の対象）。

既定値もバイアスを補強する: `pass_threshold = 0.6`・`min_samples = 1` により、ほぼ信頼できる類似成功が1件あれば安いティアを信頼し、Thompson サンプリングの探索は安いティアに小さなボーナスを与えて未検証の安ティアを優先的に試す。

## ライセンス

MIT
