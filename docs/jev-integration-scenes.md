# jev 統合シーン計画 — どこに配線してよいか、どこに恒久的に配線しないか

> 測定日 **2026-10-02**、測定点 **`5aadf2cf`**。jev の API 事実は
> docs.typesafe.ai から同日取得したもの。数字を引用するときはこの測定点を併記すること
> （CLAUDE.md「測定値」節）。

`jev` は TypeSafe AI の "System One" モデル。**テキストを生成せず、型付き・較正済みの
判定**を返す。実装ラッパは [`crates/jev`](../crates/jev/README.ja.md)。

この文書は「jev をどこで使えるか」の正典である。**Tier C は恒久的な除外リスト**であり、
`/flow` や condukt がここを読んで配線先を選ぶ。

---

## 0. 支配的制約 — なぜ Tier が必要なのか

jev は**外部のクローズドなクラウド API**（課金制・ホスト側で版が変わる）。

**CLAUDE.md 第7節**: 外部サービスを使ってよいのは**参考意見（advisory）まで**であり、
**流れを止める/通す権限そのものは常に手元に持ち続ける**。外部に預けると
**権限・可用性・可視性の 3 つを同時に相手に握られる**。

したがって本文書の全体を貫く判定基準は 1 つだけ:

> **jev の答えが「無かったこと」になっても、結果が変わってはならない。**
> 変わるとしたら、それは**エスカレーションが 1 つ減る**という形だけである。

ラッパ側はこれを型で強制している（`Advice` に `Clean` が無い／`Verdict` と結合しない／
キー不在ならネットワーク呼び出し 0 回）。**だが型が守るのはラッパの境界までで、
配線先の選択は守らない。** それがこの文書の役割である。

### もう 1 つの理由 — jev の答えは観測ではなく予測である

CLAUDE.md 第2節は「**判断は予測にすぎない。fail はテストで決着させる**」と定める。
jev が返す較正済み確率は、よく較正された**予測**であって**観測**ではない。
したがって jev の出力を「事実の棚」に置いてはならない。ランキングや注記は
予測を予測として使う用途であり、ゲート判定は予測を事実として使う用途である。

### 3 つ目の理由 — 入力が攻撃者の影響下にある

実測された落とし穴（下記 §3）に
「**自分の分類を主張する敵対的テキストで答が動く**」がある。jev に渡す `state` は
diff・チケット・プロンプト・transcript 断片・コマンド行であり、いずれも
攻撃者が書ける可能性がある。**攻撃者が答を動かせる信号に block/allow を預けてはならない。**

---

## 1. Tier A — 順序付けのみ（判定ゼロ）

jev に渡す権限は**並び順だけ**。どの項目も、jev が無いときは**従来の順序**に戻るだけで、
項目が消えたり増えたり、合否が変わったりしてはならない。

| # | シーン | 使うプリミティブ | jev に渡す権限 | jev 不在時 |
|---|---|---|---|---|
| A1 | **`scout`** 5レンズの施策スコアリング／タグ分類（[`backlog-tag-taxonomy.md`](backlog-tag-taxonomy.md)）／既存 backlog との重複検出 | Score（影響度）+ Choice（タグ）+ Noul（重複か） | 並び順とタグ候補のみ | 既存の `harness_core::scorer` の順序そのまま |
| A2 | **`backlog`** triage — 新規起票の重複検出・weight 提案 | Noul（同一課題か）+ Score（weight） | 並び順のみ。`priority` は上書きしない | 既存の (priority, created_at) 順 |
| A3 | **`overwatch review-queue` / `continuous-audit`** の finding 意味的重複排除・重大度順 | Noul（同じ欠陥か）+ Score（重大度） | 並び順のみ。**CONFIRMED/REFUTED/UNVERIFIED の三値判定には触らない** | 既存のキーワード一致 dedup |
| A4 | **`ctxrot` / `context-governor` / `playbook`** の注入候補の関連度 | Score（関連度） | 予算内で何を先に入れるかの順序のみ | 既存の `retrieval` / `text_index` スコア |
| A5 | **`stuckguard`** の near-repeat（現 Jaccard）への**追加**エスカレーション信号 | Noul（同じ試行の繰り返しか） | **上げる方向のみ**。Jaccard が出した警告を消せない | Jaccard 単独の判定そのまま |

### A3 に特に注意

`continuous-audit` は **finder と verifier のモデル多様性を MUST** とする
（CLAUDE.md 第6節: 生成と検証が盲点を共有すると検証は儀式になる）。jev を
**verifier 側に置いてはならない** — dedup とランキングは finding の*集合の整理*であって
*検証*ではない。その境界を越えると、第6節が名指しする「敵対的レビュー自身の fail-open」
を外部サービスに作ることになる。

### Tier A に配線するときの共通チェック

1. jev の答えを**捨てた場合のコードパス**が存在し、テストされているか。
2. 順序が変わるだけで、**集合の要素数が変わらない**か（dedup は例外だが、
   その場合「消される側」は破棄ではなく**マージ記録**として残すこと）。
3. キー不在で**ネットワーク呼び出しが 0 回**であることを、その消費者側でも
   `RecordingTransport` で数えているか（`crates/jev/tests/no_key_no_network.rs` が手本）。

---

## 2. Tier B — 人間に見せる注記のみ

jev に渡す権限は**テキストだけ**。判定の値を一切動かさない。

| # | シーン | 使うプリミティブ | jev に渡す権限 | jev 不在時 |
|---|---|---|---|---|
| B1 | **`blastguard`** が構文解析できず `Ask` にした時の「なぜ危険そうか」の理由文 | Noul / Choice | **テキストのみ**。`Ask→Allow` も `Ask→Deny` もしない | 現在の `Ask` 文面そのまま |
| B2 | prompt-injection の**追加**警告 | Noul（これはエージェント宛の命令か） | **警告の追加のみ**。既存の警告を消せない | 既存の決定論検出のみ |
| B3 | **`specguard brief`** の second opinion | Noul（この正典ルールはこのタスクを覆うか） | **注記のみ**。`covered` は出せない | 既存の三値 verdict のみ |

### B1 — `Ask→Deny` も禁止である理由

直感に反するので明示する。`Decision::Ask` は
`crates/blastguard/src/model.rs:9` のとおり「**コマンドについての判定ではなく、
判定を推測することの拒否**」である。jev に `Ask→Deny` を許すと、
**外部サービスが deny 権限を持つ**ことになり第7節に反する。`Ask→Allow` は
古典的な fail-open で第3節に反する。**残るのは理由文を添えることだけ**であり、
それは実際に有用である（人間が Ask に答えるための材料になる）。

### B3 — `covered` を出せない理由

`specguard brief` の `covered` は**許容側**である（divert せず実装へ進む）。
`not-covered` は書き込み側（specforge に spec を起こさせる）。したがって:

- jev が `covered` を出せると → 正典が無いのに「ある」と言える = fail-open。
- jev が `not-covered` を出せると → 既に正典がある領域に二重の spec を起こせる。

**どちらも駄目**なので、jev は「この引用ルールはタスクに関連しそうだ／しなさそうだ」
という**注記**しか出さない。verdict は既存の決定論ロジックと人間が決める。

---

## 3. jev API の実測事実（2026-10-02、docs.typesafe.ai）

配線する人が毎回調べ直さないための記録。**数字を転記するときは測定点も一緒に運ぶこと。**

- エンドポイント: `POST https://api.typesafe.ai/v1/systemone`
- 認証: `Authorization: Bearer <key>` / 環境変数 **`TYPESAFE_API_KEY`**
- リクエスト: `{"state": <string|object|array>, "model": "jev-latest", "questions": {...}}`
- プリミティブ:
  - **Noul** — 「これは真か」→ 確率 0–1。`criteria` 任意。**応答に `confidence` フィールドは無い**（確率そのものが答え）
  - **Choice** — `criteria` 必須 map（選択肢名→説明、最大 255）→ `choice` + `probabilities` + `confidence`
  - **Score** — `criteria` 必須 array（2–10 段）→ `score` + `legend` + `probabilities` + `confidence`
- レスポンス: `{"model": ..., "answers": {...}, "usage": {"input_tokens": N, "output_tokens": N}}`
- **1 リクエストに複数問を入れても並列評価され、レイテンシはほぼ変わらない**（問いを足すのは安い）
- エラー: `401 Unauthorized` / `422 Unprocessable Entity` / `429 Too Many Requests` / `529 Overloaded`。`429` と `529` は指数バックオフ推奨
- 制限: 64k ctx（state 32k + 最長の question）、1,200 req/min、250k tok/s
- 価格: 入力 **$0.042/M tokens**、出力無料

### 落とし穴（隠さずに運ぶ）

- **数え上げ**は規模とともに誤差が増える → コードでループする
- **日付演算**は不可 → Choice で年月日を抽出しコードで計算
- **同じ問いを Noul と Choice で聞くと不整合**（否定の和が 1.0 にならない）→ 混ぜて比較しない
- **自分の分類を主張する敵対的テキストで答が動く** → §0 の 3 つ目の理由
- モデルは**字面どおり**答える（意図を読まない）
- `noul = 0.5` は「中程度」ではなく「**等確率**」→ スペクトラムには Score
- **`jev-latest` は閾値ドリフトする** → 閾値を調整する前に具体版へ固定する
  （`~/.jev/config.toml` の `model`）。ラッパは**レスポンスが報告した** model を
  ledger に記録するので、実際に何が答えたかは観測可能

---

## 4. Tier C — 恒久的に配線しない（除外リスト）

**ここに挙がっているものへ jev を配線する変更は、レビューで却下されるべきである。**
「advisory として足すだけだから」も却下理由に含む — 一度入れば次の変更が権限を広げる。

| 対象 | なぜ恒久的に対象外か |
|---|---|
| `donegate` / `reviewgate` / `propguard` / `tdd` / `precommit-audit` / `budgetguard` | Stop ゲート＝**block/allow の権限そのもの**。第7節 |
| `parallelguard` | PreToolUse の deny 権限。判定不能を deny に倒す設計で、外部の可用性に依存させられない |
| `blastguard` の **判定部分** | 同上。理由文（B1）だけが許される |
| `condukt` の完了ゲート・`schedule.rs` | 決定論スケジューラ。ここに予測を入れると「並列が危険なら直列」の保証が壊れる |
| **F→P オラクル**（`condukt state check-oracle`） | RED→GREEN の**観測**を要求する機構。予測で代用した瞬間に空虚になる（第2節） |
| `.githooks/**`（pre-commit / pre-push） | stdlib-only python で**ネットワーク非依存**を維持する。第7節は「CI・ホスト固有 API が無いと成立しない設計にしない」とも定める。ゲートの実体は local に置き、**ネットワークを前提にしない** |
| `specguard brief` の **verdict** | §2 の B3 参照。`covered` も `not-covered` も出させない |
| `schemaguard` の schema 検証 | 宣言 schema との照合は決定論であるべき。「だいたい合っている」は検証ではない |

### 一般則

**次のどれかに当てはまるなら Tier C である:**

1. その出力が **block / allow / pass / fail** を決める。
2. その出力の**欠落**が「問題なし」と読まれうる。
3. その機構が「**観測**された事実」を要求している（オラクル・ゲート・テスト）。
4. それが**ネットワーク非依存**であることを売りにしている（`.githooks/`）。

---

## 5. 現状

**実装済み**: ラッパ（`crates/jev`）のみ。バージョンはここに転記しない（転記した数字は次の bump で腐る）— `crates/jev/Cargo.toml` を見ること。

**未着手**: Tier A の 5 件と Tier B の 3 件は**どれも配線されていない**。
ラッパ API が固まる前に消費者を作らないという 2026-10-02 のユーザー裁定により、
個別の backlog item として起票済み。`/flow` が次の周回で拾う。
