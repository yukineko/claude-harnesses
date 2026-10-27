# DESIGN: compass — ゴール再接地と「次の一手」導出 → condukt 受け渡し

**プロジェクトのある時点で「次に何をしたらいいか分からなくなる」瞬間に、
ゴール（北極星）と完成定義を彫り直して鮮明にし、現状との gap を出し、
焦点に合う一手だけを condukt へ渡し、それ以外は保留へ流す再オリエンテーション層。**

このドキュメントは harness 一家の新規プラグイン `compass` の設計。
判定の根（棄却ループによる "定義を彫る"）は `consulting-agent`
（`/mnt/c/Users/hiroyuki_nakayama/src/consulting-agent`）から抽出し、
specguard の specforge intake（[../specguard/DESIGN-INTAKE.md](../specguard/DESIGN-INTAKE.md)）と
**同一エンジンを共有する**（二重実装しない）。実行は `condukt` に委ね、保留は route の JSON に印字するだけで保存しない（2026-10-27 ユーザー裁定）。

---

## 1. 動機と立ち位置

### 解く痛み

「次が分からなくなる」には起点が複数あるが、本プラグインが対象にするのは2つ：

- **④ ゴールが霞む** — 「このプロジェクトは今何のためか／完成とは何か」を見失う（マクロ）
- **② 一区切り後の空白** — 終わった、で次の候補が目の前に無い（ミクロ）

**②は④の症状**である。ゴールが鋭ければ、一片を終えるたび「現状とゴールの差分」が見えて
次の一片が自然に浮かぶ。空白が起きるのは、降りていく勾配（gap）が無いから。
よって compass の中核仕事は「候補を列挙する（症状治療）」ではなく
**「ゴールを鋭く保ち、次の一手をそこからの差分として導く（勾配の算出）」**。

```
次の一手 = (鋭いゴール/完成定義) − (現状: git・deepwiki・テスト)  の最大かつ右サイズな差分
```

### 既存 harness との棲み分け（再発明しない）

| 問い | 担当 |
|---|---|
| 構造はどうなっているか | `deepwiki` |
| 仕様↔実装はズレていないか | `specguard:run`（drift 監査） |
| **何のためか・完成とは何か・次の一手は何か** | ← **compass（空き地）** |
| 与えられた課題を分解・並列実行・完了ゲート | `condukt` |

`condukt-interpreter` は「与えられた課題」を構造化するだけで、**そもそも何を課題に選ぶべきかは
誰も決めていない**。compass はこの上流に座り、出力（彫れたゴール＋合意された一手）を
そのまま condukt の入力（課題）にする。**指針 → 実行のチェーン**を閉じる。

### 重さ・頻度の設計制約

④②は1日に何度も起きうる**軽い再オリエンテーション**。重い監査艦隊（drift+coverage+security）は
別の痛み（③「多すぎて選べない」）用であり、本プラグインの既定動作には**含めない**。
compass は既存シグナルを読む合成器として軽く保ち、重い監査が要るときだけ `specguard:run` 等へ escalate する。

---

## 2. 設計原則（harness / specforge を継承）

1. **subscription-native** — API キー不要。LLM 労働は本体 Claude Code（サブスク枠）に委ね、
   バイナリは状態維持と context 注入のみ。API を叩かないので 429 が原理的に起きない
   （consulting-agent フック化で実証済みの方針）。
2. **判定は LLM・ハーネスは決定的** — ゴールを彫る／gap を読む／一手を選ぶのは LLM。
   size 集約・routing・状態管理は決定的バイナリ。
3. **棄却ループ（推論で埋めず人間に詰める）** — 曖昧なゴールを機械が勝手に補完しない。
   霞んでいる点は AskUserQuestion 相当で**人間に同期で詰めて彫る**。矛盾は権威で自動解決しない。
4. **焦点保護（論点3=B案）** — 一度に進めるのは焦点に合う右サイズの一手だけ。
   大きい寄り道も小さいノイズも保留へ流し、糸を見失わせない。
5. **二重実装しない** — 棄却ループの "定義を彫る" は specforge intake と同一エンジンを共有。
   分解は condukt-interpreter、実行は condukt binary、保留は保存しない（印字のみ）。compass は薄いオーケストレータ。
6. **何度でも同意してよい（論点1）** — 合意は1回/2回の固定ゲートではなく、棄却ループの各ラウンドで
   何度でも合意し直せる。「condukt 実行可能状態」はユーザーが go と言ったラウンドで到達する。

---

## 3. アーキテクチャ

```
[compass = 新規・薄いオーケストレータ]                       [既存・無改造/小改造]

 /compass 起動
   │
   ▼
 ① charter を映す ──────────────────────────────── charter.md（新アーティファクト）
   │   霞んでいれば → 棄却ループで彫り直す  ←── consulting-agent engine ∪ specforge intake（共有）
   │   （何度でも同意し直せる：原則6）
   ▼
 ② gap を出す ─── ゴール − 現状(git/deepwiki/test) の差分
   │
   ▼
 ③ 課題化 → 分解 ──────────────────────────────── condukt-interpreter（再利用, Decomposition JSON）
   │                                                 ＋ optional `size` フィールド（論点2=i, §6）
   ▼
 ④ routing（B案：焦点保護）──────────────────────── compass の triage（新規・決定的）
   ├─ 焦点に合う右サイズの一手 ──▶ condukt schedule/worktree/state（実行・無改造）
   └─ それ以外（大きい寄り道＋小ノイズ）─▶ 保留 ─▶ route の JSON に印字のみ（保存しない）
   │
   ▼
 ⑤ パンくず ── Stop 時に「次の物理的一手」を charter に書き戻す（②空白の再発予防）
```

---

## 4. アーティファクト：charter

プロジェクト高度の「北極星＋完成定義＋差分」を持つ**生きた一枚**。
既存で最も近い specguard canon は*詳細 spec* で高度が違う（霞んだとき戻る一行の北極星ではない）ため、
別アーティファクトとして持つ。場所は対象リポの `.compass/charter.md`（condukt と同じくリポ同居）。

最小フィールド（決定的にパースできる構造を持つ Markdown）：

| 節 | 内容 | 鮮度 |
|---|---|---|
| `north_star` | 1〜2行。「このプロジェクトは究極的に何のためか」 | 霞んだら棄却ループで彫り直す |
| `definition_of_done` | 観測可能な完成条件の箇条書き（condukt の `done_criteria` と同語彙） | 同上 |
| `measuring_stick` | 物差し（§7）。次の一手を何で測るか | プロジェクト方針として固定寄り |
| `current_gap` | ゴール − 現状 の要約（compass が毎回再生成） | 毎ラウンド再計算 |
| `next_action` | 再開時の最初の物理的一手（⑤パンくずが書く） | Stop ごとに更新 |
| `parked` | 保留に回したものへのポインタ（`compass charter --write` で skill が書く。route は保存しない） | skill が書く |

charter が無い／古い／薄いときは、compass が起動時に**棄却ループで生成・彫り直す**。
推論で埋めず、鮮明化できない点は人間に同期で詰める（原則3）。

---

## 5. フロー詳細（棄却ループと "condukt 実行可能状態"）

1. **映す** — charter を read-only で提示。ゴール/完成定義が鮮明なら通過、霞んでいれば次へ。
2. **彫る（棄却ループ）** — 「ゴールはまだ X か？」「今の"完成"とは観測可能に何か？」を問い返し、
   曖昧・筋の悪い枠組みを棄却して鋭くする。**何度でも同意し直せる**（原則6）。
   consulting-agent の Choice 設計知見を流用：選択肢は具体シナリオ＋オプトアウト、
   **動機の二択は禁止（F1-d）**。
3. **gap を出す** — 鮮明化したゴール／完成定義と現状（git 直近・deepwiki・落ちてるテスト）を
   diff し、最大の差分を `current_gap` に書く。
4. **課題化 → 分解** — gap から導いた一手を free-text 課題として condukt-interpreter に渡し、
   Decomposition JSON（＋ size）を得る。
5. **同意 → routing** — `{彫れたゴール＋gap＋分解タスク＋各 size＋routing 案}` を1画面で提示し合意を取る。
   合意は go が出るまで何度でも回せる。go の時点が **condukt 実行可能状態**。
6. **パンくず** — Stop 時、本体応答から「次の物理的一手」を抽出し charter `next_action` に書き戻す
   （consulting-agent の `--hook-stop` で ` ```...``` ` ブロックをパースした方式を流用）。

---

## 6. routing：size + triage（論点2=i / 論点3=B）

### condukt スキーマ拡張（論点2=i）

condukt の Decomposition タスクに **optional な `size`** を1フィールド足す（scheduler は無視可、
triage が読む、将来 condukt 自身も使える）：

```json
{ "id": "t1", "title": "...", "touched_files": [...], "deps": [...],
  "class": "parallel|serial|gated", "suggested_model": "...",
  "done_criteria": "...",
  "size": "xs|s|m|l|xl" }          // ← 追加（optional, 既存ランは未指定でも壊れない）
```

互換性：`#[serde(default)]` で省略可にし、未指定タスクは triage 上「サイズ不明＝人間に確認」に倒す。
`condukt validate` は `size` を任意フィールドとして許容（不明値はエラーにしない）。

### triage ロジック（論点3=B：焦点保護）

分解結果から **焦点に合う右サイズの一手だけ**を condukt 実行に回し、残りは全部保留：

- **実行へ**：今のゴール/gap に最も効き、かつ単独で完結する右サイズ（既定 `s`〜`m` 1 件、
  密結合なら `condukt` の serial/gated で束ねた最小集合）。
- **保留へ**：
  - **大きい寄り道**（`l`/`xl` または gap の主筋から外れるもの）→ いま着手すると焦点を失う。
  - **小さいノイズ**（`xs` の散発、主筋に効かない雑務）→ フローを切ってまで今やらない。

保留は route の JSON に印字するだけで、どこにも保存しない。次回 `/compass` の gap 入力にも再浮上しない
（2026-10-27 ユーザー裁定で `.claude/progress.md`「残り」への書き戻しと自己供給ループを撤去）。

> 注：size の*向き*は B 案＝焦点保護を既定とするが、閾値（どこを s/m と見るか）は `.compass/config.toml` で
> プロジェクトごとに調整可能にする。

---

## 7. 物差し（measuring_stick）

「次の一手」を何で測るか。consulting-agent の事業版物差し「顧客が金を払うか」の**プロジェクト版**：

> **「私が今も擁護できるゴールに、測れるだけ近づくか」**

④②型の痛みでは、最も効くのはしばしば **build より validate**——「このゴールが今も妥当か」を
最小コストで検証する一手。よって物差しは「擁護可能性 × ゴールへの接近距離 ÷ コスト」を既定の軸とし、
charter `measuring_stick` に明記してプロジェクトごとに上書き可能にする。

---

## 8. 配置（placement）

棄却ループ engine は specforge intake と共有するが、compass の*目的*（再オリエンテーション → condukt 受け渡し）は
specguard の*目的*（spec↔impl drift 監査）と別。よって：

- **推奨**：`crates/compass` を独立プラグインとして新設し、彫り直しエンジンは
  `harness-core` もしくは specforge と共有するライブラリとして切り出して両者が参照する。
- engine を specforge と一本化する具体（どちらの crate に lib を置くか）は DESIGN-INTAKE.md の進捗に合わせて確定する。

`harness-core`（config/install/store/transcript/usage/hook）に載せ、`bin/` クロスコンパイル、
`/compass` skill、SessionStart hook（charter の鮮度リマインド）、`--hook-stop`（パンくず）で構成する。
reqwest/tokio/axum/specta は不要（API を叩かない）→ 薄い crate になる。

---

## 9. consulting-agent の扱い（会話の最終結論）

> **丸ごと統合は不要。** drift は specguard、把握は deepwiki/specguard:brief、分解は condukt が既に担う。
> 価値があるのは consulting-agent の **"定義を彫る" 棄却ループエンジンの設計知見**だけ
> （三層→物差し→棄却、Choice 設計 = 具体シナリオ＋オプトアウト・動機二択禁止 F1-d、
> `--hook`/`--hook-stop` のサブスク完結方式）。これを charter 彫り直し器として流用し、
> specforge intake と一本化する。**web/server/生 API 直叩き（axum/Next/reqwest）は捨てる**
> （サブスク完結思想に反し、且つ 429 で実用不可）。

---

## 10. 未確定 / 次の決定

- [x] **engine の置き場：解決（§11）** — interrogate ループ機構を `harness-core` の薄い generic モジュールに切り出し、specforge intake と compass の両方が parameterize する。
- [x] **charter の鮮度判定：解決（§12）** — 「霞んでいる」を C1–C5 の graded gate（決定的 floor + LLM 監査）として定義。
- [x] **size の既定閾値：解決（§13）** — interpreter 用ルーブリックで xs–xl を定義、`right_size=s,m` を condukt へ。
- [x] **`/compass` 起動経路：解決（§14）** — skill 主 ＋ SessionStart nudge ＋ Stop パンくずの3点、prefix hook 無し。

---

## 11. 共有する interrogate engine と配置（論点1 の specforge すり合わせ）

specforge intake（[../specguard/DESIGN-INTAKE.md](../specguard/DESIGN-INTAKE.md) §5）の interrogate ループと
compass の charter 彫り直しは、**機構が同型**である。二重実装しない（原則5）ため、機構だけを切り出して共有する。

### 共有する / しないの境界

| | 共有（generic engine） | specforge ローカル | compass ローカル |
|---|---|---|---|
| **何を彫るか（対象）** | — | 要件/spec（要望→IR） | プロジェクトの北極星＋完成定義（charter） |
| **gather ソース** | — | Obsidian>repo canon>prompt（3ソース） | charter + git 直近 + deepwiki |
| **rigor gate 定義** | — | G1 接地 / G2 沈黙 / G3 矛盾 / G4 反証可能 | C1–C5（§12） |
| **出力** | — | 要望素材束 → normalize | 彫れた charter → gap 導出 → condukt routing |
| **ループ駆動** | ✅ `while open_qs && round<max: ask → 再判定 → 更新` | ← 使う | ← 使う |
| **open_question モデル** | ✅ `{ gate, ref, gap, sources, default }` | ← 使う | ← 使う |
| **tiebreak-default-first** | ✅ 既定を先頭の"推奨"選択肢に | ← 使う | ← 使う |
| **hybrid sentinel フォールバック** | ✅ `max_rounds` 到達/defer で sentinel→離席 | ← 使う | ← 使う |
| **問い方規律** | ✅ 未達は機械が出す・問い方は LLM・矛盾は人間が裁く（原則5）・動機二択禁止(F1-d) | ← 使う | ← 使う |

**境界線**：compass と specforge は「**どう彫るか（engine）**」を共有し、「**何を彫るか（対象）・どの gate・何を gather**」は
共有しない。両者は互いを呼ばず、**ともに `harness-core` を呼ぶ**（依存方向を一方向に保つ）。

### 配置：`harness-core` に generic な interrogate モジュール

`harness-core`（ビルド時依存・各バイナリに静的に焼き込まれる）に薄い generic 層を置く。
ドメイン（gate / gather / 出力）はトレイト経由で各プラグインが供給する：

```rust
// harness-core::interrogate （新規・薄い制御構造のみ）
pub struct OpenQuestion { pub gate: String, pub reference: String, pub gap: String,
                          pub sources: Vec<Fragment>, pub default: Option<String> }

pub trait RigorGates {                       // ドメインが実装（specforge=G1-4, compass=C1-5）
    fn evaluate(&self, ctx: &Bundle) -> Vec<OpenQuestion>;   // 未達を open_q 集合に分解
}

// ステートレスな2操作（バイナリは AskUserQuestion を呼ばない）
pub fn evaluate<G: RigorGates>(gates: &G, bundle: &Bundle) -> Vec<OpenQuestion>; // 未達を返す
pub fn apply<G: RigorGates>(gates: &G, state: &mut CarveState,                   // 回答を反映し
                            answer: Answer) -> Vec<OpenQuestion>;                // 再評価して残りを返す
```

**重要（アーキ訂正）**：interrogate ループを Rust 関数が self-drive することはできない。`AskUserQuestion` は
Claude Code 側のツールであり、バイナリからは呼べない。よって **ループは SKILL（LLM）が駆動**し、
バイナリは**ステートレスな2操作だけ**を提供する（consulting-agent のフックモデルと同型）：

```
SKILL（LLM）が駆動:
  loop:
    open_qs = compass evaluate              # バイナリ: gates.evaluate(bundle) → 未達
    if open_qs 空 or round>=max: break
    ans = AskUserQuestion(open_qs.next())   # ← LLM 側ツール。既定を先頭"推奨"・自由入力可（§5,§3.1）
    compass apply --answer=ans              # バイナリ: 永続化(harness-core::store) + 再評価
  残り open_qs → sentinel（離席）/ 解決 → gap 導出へ
```

- round カウント・defer・sentinel は `state`（`harness-core::store`）にバイナリが永続化し、
  呼び出しをまたいで保つ。`max_rounds=0` で「同期せず全部 sentinel」（後方互換）。
- `Interrogator` トレイトは廃し、問い方規律（具体シナリオ＋オプトアウト・動機二択禁止 F1-d・矛盾は人間が裁く）は
  **SKILL の prompt と OpenQuestion の構造（default/options）**で担保する。バイナリは文面を持たない。

- **harness-core に入れてよい理由**：これは「未達検出→人間に詰める→再判定」という**再利用可能な制御構造**であり、
  特定ドメインに依存しない。gate・gather・出力（ドメイン）はトレイトで外出しするので harness-core は薄いまま。
  既存の harness-core（config/hook/install/store/transcript/usage）に並ぶ「対話制御プリミティブ」として収まる。
- **早すぎる抽象化の回避**：specforge intake はまだ DESIGN 止まり（forge に `gather.rs` はあるが `interrogate.rs` は未実装）。
  よって**後付け refactor でなく、最初から両者がこの generic engine に乗る**形で書ける。compass の interrogate を
  `harness-core::carve` で実装し、specforge intake 実装時に同じ engine へ寄せる（[[harness-core-migration]] の
  「共有 vs domain-local」線引きに沿う）。
- **consulting-agent からの流用**：問い方規律（具体シナリオ＋オプトアウト・動機二択禁止 F1-d）と
  `--hook-stop` の ` ```...``` ` ブロックパース方式は、`Interrogator` 実装と⑤パンくずに流用する。

---

## 12. charter 鮮度ゲート C1–C5（論点2 の「霞んでいる」の決定的定義）

「ゴールが霞んでいる」を、specforge の graded rigor gate（G1–G4）と同じ枠組みで **C1–C5 の未達集合**として定義する。
**machine floor（決定的）→ LLM 監査** の二段（specforge §4 の floor+D2 を踏襲）。**未達 0 件 = charter は鮮明**。
1件でも残れば §11 の carve ループ（interrogate）へ。

| ゲート | 判定 | 担当 | 未達 → 何を詰めるか |
|---|---|---|---|
| **C1 存在** | charter.md があり north_star/DoD が空でない | 決定的 | 無 → ゼロから彫る |
| **C2 鮮度（drift）** | charter が現実から乖離していない（下記の決定的信号） | 決定的 | 乖離 → 「ゴールはまだ妥当か」を確認 |
| **C3 観測可能** | DoD 各項目が観測可能な pass/fail（specforge G4 と同型） | LLM | 曖昧 → 測定基準を要求 |
| **C4 整合** | north_star/DoD が直近の実作業と矛盾しない（G3 と同型） | LLM | 矛盾 → 「ゴールが移ったか」を人間が裁く（原則5） |
| **C5 勾配可能** | gap（ゴール − 現状）が計算可能な具体度を DoD が持つ | LLM | 抽象すぎ → 次の一手が引ける粒度まで具体化を要求 |

### C2 鮮度の決定的信号（cheap floor）

LLM を使う前の安価な floor。いずれか1つでも閾値超過なら「drift 疑い」として C3–C5 へ進む（config で閾値調整）：

1. **コミット乖離** — charter 最終更新（commit/mtime）以降のコミット数 > `stale_commits`（既定 20）。
   episodic なプロジェクトでは wall-clock より信頼できる主信号。
2. **経過時間** — 最終更新からの経過 > `stale_days`（既定 14）。副信号。
3. **DoD 参照の消失** — DoD/charter が参照するパス・シンボルが現存するか（決定的なファイル存在チェック）。
   消えていれば強い stale 信号。
4. **next_action 乖離** — ⑤パンくずが書いた `next_action` と、その後に実際コミットされた内容が食い違う
   （記録した次の一手と違うことをやった＝糸が動いた＝charter が古い可能性）。

### 起動経路と鮮度の関係（cheap-first）

- **SessionStart hook**：C1/C2（決定的のみ）を走らせ、drift 疑いなら「charter が古いかも、`/compass` を」と**nudge するだけ**
  （LLM 不使用・ブロックしない、beacon/stuckguard と同じく軽い）。
- **`/compass` skill**：C1–C5 全部（LLM ゲート含む）を走らせ、未達があれば carve ループへ。ユーザーが「次どうする」と
  問うている瞬間なので、LLM ゲートのコストを払う価値がある。

### config（`.compass/config.toml`）

```toml
[freshness]
stale_commits = 20     # charter 最終更新以降のコミット数しきい（主信号）
stale_days    = 14     # 経過時間しきい（副信号）
check_dod_refs = true  # DoD 参照パス/シンボルの現存チェック
[carve]
max_rounds    = 4      # interrogate 同期ラウンド上限。0 = 全部 sentinel（後方互換）
[routing]
right_size    = ["s", "m"]   # B案：これを「右サイズの一手」とし condukt へ。他は保留（§6）
```

---

## 13. size ルーブリックと routing のエッジ（論点3=B の確定）

size は condukt-interpreter が分解時に付与する（§6 のスキーマ拡張）。LLM が一貫して付けられるよう、
**touched_files の広さ・deps・関心の数**に紐づくルーブリックを interpreter prompt に与える：

| size | 目安 | routing（B案・焦点保護） |
|---|---|---|
| **xs** | 1 ファイル・deps 無し・自明（typo/設定値） | 保留（ノイズ。フローを切らない。まとめて後で） |
| **s** | 1–2 ファイル・dep ≤1・単一関心・1 セッション未満 | **condukt へ（右サイズ）** |
| **m** | 3–5 ファイル / 1 モジュール・1 セッション | **condukt へ（右サイズ）** |
| **l** | 複数モジュール / 横断・複数セッション | 保留（大きい寄り道。焦点を失う） |
| **xl** | それ自体が再分解を要する（プロジェクト内プロジェクト） | 保留（→ ゴールを小さく彫り直す対象） |

### エッジケース（compass の出力状態）

- **右サイズが複数** → B案は焦点保護なので、gap の主筋に最も効く**1件だけ** condukt へ、残りは保留。
  （condukt 自身は並列実行できるが、compass は「今コミットする一手」を1つに絞る＝糸を増やさない。）
- **右サイズが 0（全部 xs か全部 l/xl）**：
  - 全部 **l/xl** → 「ゴールが大きすぎて右サイズの一手が無い」→ **carve ループに戻りゴール/DoD をより小さく彫り直す**
    （= 物差し §7：擁護できるゴールへ"測れるだけ"近づく最小スライスを探す。多くは validate 系）。
  - 全部 **xs** → 「主筋に効く一手が無い＝今は実質完了 or 方向が尽きた」→ charter の north_star 自体を問い直す合図。
- **密結合した s/m 群が1つの関心** → condukt の serial/gated で束ねた**最小集合**を1単位として渡す（focus は保つ）。

### condukt への受け渡し形

compass の go 時点の出力 = **合意された一手の課題文 ＋ 文脈（north_star / current_gap / measuring_stick）**。
これを condukt-interpreter にそのまま渡す（compass は分解を再実装しない）。skill 連携として
`/compass` の最終ステップで、合意された課題を添えて `/condukt` を起動する（または起動を促す）。

---

## 14. 起動経路（論点4 の確定）

consulting-agent の `相談:`/`consult:` prefix は**持続的対話**（ペルソナを張り続ける）向けだった。
compass は**意思決定点での episodic な問い**（「次どうする？」）であり、持続セッションモデルは過剰。
よって **prefix hook は持たない**。構成は harness 標準（少数の hook ＋ skill）に揃える：

| 経路 | 役割 | LLM | ブロック |
|---|---|---|---|
| **`/compass` skill** | 主入口。C1–C5 全ゲート → carve → gap → routing → condukt 受け渡し | 使う | しない |
| **SessionStart hook** | C1/C2（決定的のみ）で drift 疑いを検知し「charter が古いかも、`/compass`」と nudge | 不使用 | しない |
| **Stop hook（`--breadcrumb`）** | 本体応答から「次の物理的一手」を抽出し charter `next_action` へ書き戻す（⑤予防） | 不使用 | しない |

consulting-agent の `--hook`（UserPromptSubmit 注入）は採らない。注入を持続したい持続対話ではないため。
`--hook-stop` のブロックパース方式だけ `--breadcrumb` に流用する。

---

## 15. 実装計画（段階 C → B → A）

specguard/specforge の段階導入（DESIGN-INTAKE §10-5）に倣う。**動く最小から、context を食いつぶさず刻む。**

### 段階 C — PoC（Workflow、crate を作らない）
carve ループの形と AskUserQuestion の UX を安価に実証する。1本の Workflow スクリプトで
`gather → C1–C5 ゲート → interrogate（1–2問）→ gap → size triage → condukt 受け渡し` が閉じるか確認。
- 入力：手で用意した最小 charter.md ＋ 実リポの git（当時は progress.md も入力だったが 2026-10-27 に撤去）。
- 検証：右サイズ抽出と「右サイズ0」エッジ（§13）が期待通り分岐するか。
- ここで `harness-core::carve` の I/F（§11）と C ゲートの未達→open_q 変換（§12）を固める。

### 段階 B — Rust 結晶化
安定した部分を crate 化。

```
crates/compass/
  Cargo.toml                 # deps: harness-core, serde, serde_json, anyhow。reqwest/tokio/axum/specta は持たない
  src/                       # （実装後の実態）
    main.rs                  # subcommands: nudge / breadcrumb / evaluate / apply / carve-reset / gap / route / charter
    config.rs                # .compass/config.toml（§12）
    charter.rs               # .compass/charter.md の read/write/parse（§4）＋ charter --write 用 serde
    gather.rs                # charter + git 直近 + deepwiki → Bundle
    gates.rs                 # RigorGates 実装：C1/C2 のみ（決定的）。C3–C5 は SKILL 側（§11/§12）
    freshness.rs             # C2 決定的信号（nudge から単独呼び出し可）
    carve.rs                 # CarveState 永続化（.compass/carve-state.json）＋ evaluate/apply の JSON view
    gap.rs                   # gap 入力の決定的組み立て（意味的 diff は SKILL）＋ current_gap 書き戻し
    route.rs                 # size triage（§13）→ condukt 課題文 / parked は印字のみ
    breadcrumb.rs            # Stop hook：```compass-next``` ブロック抽出 → charter next_action

crates/harness-core/src/
    interrogate.rs           # 新規・generic：OpenQuestion/Fragment/Bundle/RigorGates/Interrogator/carve()（§11）

crates/condukt/src/...       # Task に optional `size`（#[serde(default)]）追加、validate で許容（§6）
```

### 段階 A — プラグイン完成
`hooks/hooks.json`（SessionStart=nudge / Stop=breadcrumb）、`skills/compass/SKILL.md`、
`.claude-plugin/plugin.json`（明示 version）、`bin/` クロスコンパイル（darwin-arm64/x86_64・linux-x86_64）、
`scripts/build-plugin-bin.sh`、README。marketplace 登録はリポ root の `.claude-plugin/marketplace.json` に git-subdir エントリ追加（別リポではなくこの repo で配布）。

### 実装タスク順（依存順）
1. **condukt `size` 追加**（最小・独立・後方互換）— 他に影響せず先に入れられる。
2. **harness-core `interrogate.rs`**（generic carve engine）— compass と specforge の共通基盤。
3. **compass crate scaffold**（Cargo.toml / plugin.json / config.rs / charter.rs）。
4. **gather + gates（C1–C5）+ freshness** — carve に食わせる入力と判定。
5. **gap + route** — 差分導出と size triage、condukt 受け渡し / parked 印字。
6. **hooks（nudge / breadcrumb）+ skill SKILL.md** — 起動経路（§14）。
7. **bin クロスコンパイル + README + marketplace 登録**。

> 段階 C の PoC を 1 で着手する前に挟むと、2 の I/F が実証で固まってから結晶化できる（手戻り最小）。
```
