# Agentic Coding Guideline — condukt を背骨にプロジェクトを回す

このハーネスでプロジェクトを回す前提は **condukt が実行のオーケストレータ
(背骨)** であること。condukt が「分解 → スケジュール → worktree 並列実装 →
検証 → 完了ゲート」を決定的に駆動し、他のプラグインはその各フェーズに **差し込む
部品**として働く。

> **原則**: judgement は LLM (interpret / implement / verify)、決定的な作業
> (schedule / worktree / state / gate) は condukt バイナリ。人間は **agree と
> gated タスクの承認**という境界にだけ座る。

---

## condukt の 7 フェーズ (背骨)

```
1. interpret   要望 → Decomposition JSON           (condukt-interpreter agent)
                 task = {id, title, touched_files[], deps[],
                         class: parallel|serial|gated, done_criteria}
2. validate    condukt validate --file <json>
3. schedule    condukt schedule --file <json>       (touched_files の衝突解析で
                 parallel バッチ/serial/gated に分解。warnings を人間に提示し agree)
4. init        RID=$(condukt state init --file <json>)
5. implement   各 task: worktree create → state set running
                 → condukt-worker (worktree 内で実装) → state set done   ※並列 max 4
6. verify      condukt-verifier が done_criteria を照合 → state set verified|failed
7. gate        condukt state gate --run $RID         (全 verified かつ worktree が
                 clean/消去済みのときだけ exit 0) → worktree merge → remove → cleanup
```

state は `~/.condukt/state/<project>/<run-id>.json`、worktree は
`~/.condukt/worktrees/<topic>`。SessionStart で `condukt restore` が未完 run と
orphan worktree を警告する。

---

## 各フェーズに差し込む部品

condukt の素のフローに、ハーネスの他プラグインを **フェーズ境界で噛ませる**。

### 前段 (Phase 1 interpret の前) — よい分解のための地ならし
| 部品 | 役割 | condukt への効き方 |
|---|---|---|
| **deepwiki** `/deepwiki` | リポジトリ構造 wiki | interpreter が `touched_files` を正確に当てられる → 衝突解析が効く |
| **playbook** (自動) | 関連 curated note を注入 | interpreter / worker の判断材料 |
| **runbook** `!macro` | 手順マクロ展開 | 定型タスクの decomposition を安定化 |
| **specforge** `draft`→`ratify` | 要望 → 厳格な Spec IR | **これが condukt の入力を作る** ↓ |

### specforge → condukt のブリッジ (最重要)
**仕様駆動で回すなら、specforge の ratified spec を condukt の Decomposition に
落とす。** これで「厳格な仕様」と「決定的な並列実装」が 1 本に繋がる:

```
specforge requirement   →   condukt task
  requirement.id        →     task.id
  requirement.statement →     task.title
  requirement.canon/area→     task.touched_files   (衝突解析の入力)
  requirement.acceptance→     task.done_criteria   (verifier の照合点)
```

- `specforge draft → ratify` で **rigor ゲートと人間合意**を通った spec だけが
  condukt に入る (未 ratified は実装に進めない、という HOTL を前段で担保)。
- `specguard` (D2) で spec の沈黙/矛盾を監査してから decomposition する。
- 実装そのものは **condukt が回す** (specforge の `implement` は使わず、condukt の
  worktree 並列に一本化する。二重の worktree 機構を持たない)。

### Phase 5 implement と並走 — 自走を守る常時ガード (全自動)
| 部品 | 役割 |
|---|---|
| **ctxrot** | 各 worker の context 肥大を検出・退避・蒸留 |
| **stuckguard** | worker の反復・edit thrash を検出して介入 |
| **budgetguard** | run 全体のコスト上限で Stop をブロック |
| **gauge** | トークン・コスト・遅延を記録 |
| **taskprog** | `.claude/progress.md` に run 進捗を追跡 (跨セッション) |

### Phase 6 verify — condukt-verifier の「検証コマンド」に使う
condukt-verifier は `done_criteria` を照合するが、その中身に既存ゲートを流用する:
| 部品 | verify での役割 |
|---|---|
| **donegate** | 受け入れコマンド (test/build/lint) が全 green か |
| **tdd** | 具体テストが RED→GREEN を辿ったか (テストなし実装をブロック) |
| **propguard** | `done_criteria` から導いた意味的不変条件を保つか (閾値未満は fail-closed で Stop ブロック) |
| **precommit-audit** | worktree の diff に静的監査 (secret/禁止 API/規約) |
| **reviewgate** | diff のコードレビュー |
| **specguard** (D1) | 実装 ↔ 仕様の drift 監査 (specforge 由来の spec と照合) |

→ これらが緑になって初めて `state set verified`。落ちたら `failed` で **その task
だけ** Phase 5 に差し戻す (他の verified task は触らない = condukt の差分再実行)。

### Phase 7 gate — condukt のゲート + 人間合意
| 部品 | 役割 |
|---|---|
| **condukt state gate** | 全 verified + worktree clean を機械判定 (merge の前提) |
| **specforge** `evidence`→`agree` | 成果物別 typed evidence + **人間合意** (gated/大物の HOTL) |
| **beacon** | gate 通過 or escalation を人間に通知 (離席可能に) |

### 横断・引き継ぎ
| 部品 | 役割 |
|---|---|
| **harness-status** `/status` | condukt の run 状態 + コスト + 進捗を集約表示 |
| **difflog** | SessionEnd に run の差分サマリを書き出し / `/difflog` ナラティブ |
| **session-insights** | セッション metrics を記録 / `/record` |

---

## 実行手順 (打つコマンド)

フェーズの意味は上の表が真実源。ここはその通りに **実際に打つコマンド列**だけを置く。

### Step 0 — 一度だけ (セットアップ)
```bash
condukt --version          # 無ければ /plugin install condukt@yukineko
condukt init               # ~/.condukt と config.toml を作成
condukt install            # SessionStart hook を settings.json に統合
```
`~/.condukt/config.toml` を案件に合わせる (**衝突制御の要**):
```toml
worktree_base  = "~/.condukt/worktrees"   # 必ずリポジトリ外
default_branch = "main"
max_parallel   = 4
shared_globs   = ["**/models.py", "**/migrations/**", "docs/glossary.md"]
```
> `shared_globs` に「全体に効くファイル」を列挙すると、それに触るタスクを `schedule` が自動で
> **serial 降格**する。並列ワーカー競合を機械的に防ぐ唯一の設定。案件開始時に必ず埋める。

### Step 1 — 仕様を固める ← 人間ゲート① (フル構成のみ)
素早く回すなら飛ばして Step 2 へ。
```bash
specforge draft  --id feat-x --req req.md --canon docs/x.md#spec
specguard <audit>                        # D2: 仕様の沈黙/矛盾を監査
specforge ratify --id feat-x -m "受け入れ条件に合意"   # ratified spec だけが次へ進める
```

### Step 2 — `/condukt` を起動 (ここから自走)
```
/condukt feat-x の ratified spec を実装して      # 仕様なしなら /condukt <課題文>
```
これ一発で Phase 1–7 が自動駆動する。中で叩かれる condukt コマンドと**人間の関与**:

| Phase | 中で走る | 関与 |
|---|---|---|
| 1 解釈 | interpreter → Decomposition JSON | — |
| 2 検証+スケジュール | `condukt validate --file d.json` / `condukt schedule --file d.json` | — |
| 3 **合意** | スケジュール結果 (batches/serial/gated/warnings) を提示 | **ゲート②** agree |
| 4 init | `RID=$(condukt state init --file d.json)` | — |
| 5 並列実装 | `worktree create`→`state set running`→worker→`state set done` (max 4) | — |
| 6 検証 | verifier が `done_criteria` 照合 → `state set verified\|failed` | — |
| 7 ゲート+統合 | `condukt state gate --run $RID` → `worktree merge`→`remove`→`cleanup` | — |

`--dry-run` を付けると Phase 3 のスケジュール提示で**止まる** (分割案だけ見たいとき)。

### Step 3 — 証拠を確認 ← 人間ゲート③ (フル構成のみ)
```bash
specforge evidence --id feat-x           # 要件別に test/build 結果を集約
specforge agree    --id feat-x -m "証拠確認"
```

---

## 進捗・状態の確認 (いつでも)

```bash
condukt state list                 # 開いている run: run_id  done/total  goal
condukt state show --run $RID      # その run の全タスク状態 (JSON)
condukt worktree list              # 残っている worktree
/status                            # run状態 + コスト + progress.md を集約表示
```
セッションを跨いでも、次回 SessionStart で `condukt restore` が**未完 run と orphan worktree を警告**する。

## 手動でフェーズを叩く (デバッグ / スキル不使用時)

```bash
condukt validate --file decomp.json
condukt schedule --file decomp.json          # batches/serial/gated/warnings を目視
RID=$(condukt state init --file decomp.json)

# 1タスク分 (バッチ内は複数同時)
WP=$(condukt worktree create --topic t1 --branch condukt/t1)
condukt state set --run $RID --task t1 --status running --worktree "$WP" --branch condukt/t1
#   …WP 内で実装・commit…
condukt state set --run $RID --task t1 --status done
condukt state set --run $RID --task t1 --status verified    # done_criteria が緑なら

condukt state gate --run $RID                # exit 0 になるまで完了宣言しない
condukt worktree merge  --branch condukt/t1
condukt worktree remove --path "$WP" --branch condukt/t1
condukt worktree cleanup                     # orphan 無しを確認
```

## 失敗タスクの差し戻し

verifier が落としたら **そのタスクだけ** Phase 5 に戻す (他の verified は触らない = 差分再実行):
```bash
condukt state set --run $RID --task t3 --status failed
# 原因修正 → 同じ worktree で再実装 → done → verified
```
2 サイクルで pass しなければユーザーに判断を仰ぐ (スキルが自動でそうする)。

---

人間が手を動かすのは **3 点だけ**: ①`specforge ratify` ②Phase 3 で schedule に agree
③`specforge agree`。最小構成なら実質 `/condukt <課題>` を打って Phase 3 で 1 回答えるだけ。
間の `validate / schedule / state / worktree / gate` は全部バイナリが決定的に回す。

> **autonomy switch（完全自走）**: config の `autonomous` または env `CONDUKT_AUTONOMOUS=1`
> を立てると、`/condukt` が `condukt state autonomy-check` の exit code で分岐して
> Phase 3 の agree など**人間ゲートを縮退**する。人間の関与を外すぶん、`donegate` /
> `tdd` / `propguard` / `reviewgate` の検証ゲートを整えてから有効化する（既定は HOTL 維持）。
> verify の頑健性を上げたいときは `condukt consensus`（multi-sample 自己整合投票・opt-in）で
> N 個の verifier 判定を多数決に集約できる。

---

## 推奨ロードアウト (condukt 前提)

### 最小 — condukt + 自走ガード
```
condukt + ctxrot + stuckguard + taskprog
```
分解・並列・worktree・gate は condukt が持つので、足すのは自走の安全だけ。

### 標準 — + 検証ゲートとコスト
```
最小 + donegate + tdd + propguard + reviewgate + gauge + budgetguard + harness-status
```
condukt-verifier の done_criteria に donegate/tdd/propguard/reviewgate を流用、コスト可視化。

### フル — 仕様駆動・HOTL 完備
```
標準 + specforge + specguard + precommit-audit + deepwiki + playbook + beacon
```
要望→仕様(ratify)→分解→並列実装→検証→証拠(agree)→統合 を 1 run で閉じ、
人間は 3 つの境界にだけ座る。

---

## 一言で

- **condukt が背骨** — 分解/スケジュール/worktree 並列/state/gate を決定的に回す。
- **specforge は前段と後段** — ratified spec が condukt の入力 (task=requirement、
  done_criteria=acceptance)、最後に evidence/agree で人間合意。実装は condukt 一本。
- **既存ゲートは verifier の中身** — donegate/precommit-audit/reviewgate/specguard を
  `done_criteria` の検証に流用し、緑で `verified`、赤で **その task だけ** 差し戻す。
- **常時ガードで自走を守る** — ctxrot/stuckguard/budgetguard/gauge。
- **人間は 3 境界だけ** — ratify・schedule agree・evidence agree。残りは beacon で離席。
