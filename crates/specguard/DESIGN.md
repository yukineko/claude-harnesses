# DESIGN: specforge — 仕様生成パイプライン (specguard 兄弟ツール)

**粒度の異なる要望から、Agent 実行可能な詳細仕様 → prompt → 並列実装 までを駆動し、
各段を specguard で監査して閉じる生成側ハーネスの設計。**

このドキュメントは設計の正典。**②normalize / ③ratify / ④prompt-build / ⑤parallel-impl /
⑥evidence+agree / ⑦merge は `src/forge/` に実装済み**(`specforge draft|ratify|queue|impl-prompt|
implement|evidence|agree|merge`)。③⑥ の機械監査は specguard を無改造で呼ぶ前提のまま。
③ratify は昇格と同時に requirement を backlog へ積む(§5.4)ので、出口は散文ではなくコードにある。
`specguard`(監査=逆方向)の思想と契約をそのまま継承し、*生成=順方向* を定義する。

---

## 1. 動機と立ち位置

`specguard` は「実装が正典からずれてないか」を **read-only で逐語監査** する後半(検証側)。
本構想が欲しいのは前半(生成側):

```
①要望(粒度バラバラ) → ②詳細仕様 → ③仕様監査 → ④prompt生成 → ⑤並列実装 → ⑥実装監査 → ⑦ack/決定ログ
```

specforge は **①②④⑤** を担い、**③⑥⑦** は specguard(既存)をそのまま受け入れゲートとして呼ぶ。
両者を1つの HOTL ループに閉じるのが目的。

> 監査側 (③⑥) の findings を独立 agent で反証・網羅性批評する *検証ゲート* は
> [DESIGN-VERIFY.md](DESIGN-VERIFY.md) に別設計。specguard に閉じ込めるので、specforge ⑥ は
> 無改造で継承する(DESIGN-VERIFY.md §10)。

### 設計原則(specguard から継承・絶対に崩さない)

1. **判定は LLM、ハーネスは決定的** — specforge 本体は scope 決定・prompt 描画・プロセス
   起動・marker 解析・隔離 だけを担い、内容判断は agent に委ねる。
2. **権限で担保、お願いで担保しない** — 生成・実装 agent は書き込みが要るが、*書ける範囲を
   ハーネスが allowlist と worktree で物理的に絞る*。
3. **正典の中身はコピーしない** — prompt にはポインタだけ。生成物も「どこに書くか」で管理。
4. **生成物に勝手な権威を与えない(HOTL)** — 生成された仕様も prompt も、人間の ack を経て
   初めて正典に昇格する(specguard の prompt 批准ゲートと同型)。
5. **正典には権威の階層があるが、矛盾は権威で自動解決しない** — User 正典 > 生成ドキュメント、
   という強さの差はある。だが矛盾を検出したとき機械が「上位が勝つ」と勝手に潰してはならない。
   **矛盾は常に人間に Ask する**(§5.1)。同レベル文書同士の矛盾も同じく Ask する。
6. **生成を目的化しない** — 「何か出力する」こと自体を目的にしない。入力が不足して **厳格に
   生成できないとき**は、無理に draft をでっち上げず **人間に質問する/不足文書をリクエストする**
   (§5.2)。リクエスト時は **フォーマットや雛形まで出力してよい**(埋めてもらう型を渡す)。

### specforge と specguard の分離(なぜ別バイナリか)

| | specguard | specforge |
|---|---|---|
| 方向 | 監査(逆) | 生成(順) |
| 権限 | **read-only を強制**(Edit/Write/WebFetch deny) | 書き込み必須(隔離下で許可) |
| 出力 | findings(canon を書き換えない) | spec / prompt / 実装パッチ |
| 冪等性 | 同一入力→同一 findings | 生成は非決定的、隔離 + 監査で受け入れ判定 |

read-only 不変条件は specguard 最大の売り。これを濁さないため **specforge は別プロセス**にし、
specguard は無改造(または最小改造)で呼び出す。

---

## 2. パイプライン全体図

```
                                   ┌─────────────── HOTL ack gate ───────────────┐
                                   │                                             │
要望(issue/メモ/口頭)              ▼                                             │
   │                          [人間が昇格を承認]                                 │
   ▼                               │                                             │
┌──────────┐  ②  ┌──────────┐  ③ specguard D2  ┌──────────┐  ④  ┌──────────┐    │
│ normalize │────▶│  draft   │────────────────▶│ ratified │────▶│  prompt  │    │
│ (要望→IR) │     │  spec    │  沈黙/矛盾/重複   │  spec    │     │  build   │    │
└──────────┘     └──────────┘  不合格→差し戻し  └──────────┘     └────┬─────┘    │
                                                                       │          │
                          ⑥ specguard D1 (実装↔spec drift)             ▼          │
   ┌──────────┐◀────────────────────────────────────────────┐  ⑤ parallel impl  │
   │ converged│  drift なし → merge → 決定ログ(decide) ───────┘  (worktree×N)     │
   │ (merge)  │  drift あり → 該当 task だけ再実装(⑤へ)         ┌──────────┐       │
   └────┬─────┘                                                │ task A   │──┐    │
        │                                                      │ task B   │  │    │
        └──────────────────────── ⑦ ack ─────────────────────▶│ task C   │──┴────┘
                                                               └──────────┘
```

各段は **specguard と同じ marker/exit-code 契約**で繋ぐ(§4)。

---

## 3. 中間表現 (Spec IR)

粒度バラバラの要望を、Agent 実行可能な単位に正規化したもの。**specforge の中核データ**。
TOML/YAML で永続化し、specguard の `[[area]]` と 1:1 で対応づける(監査スコープと生成単位を一致させ、
「仕様だけ変わって誰も再照合しない」を防ぐ — specguard の change-trigger 思想と同型)。

```toml
[spec]
id = "2026-06-17-login-rate-limit"
title = "ログイン試行のレート制限"
canon = ["docs/auth.md#rate-limit"]     # specguard の area.canon と同じ書式
status = "draft"                          # draft → ratified → implementing → converged
provenance_commit = "<HEAD at draft>"     # specguard と同じく canon commit を pin

[[spec.requirement]]                       # 検証可能な原子要求(=実装 task の種)
id = "R1"
statement = "同一 IP から 60s 内に 5 回失敗したら 429 を返す"
acceptance = ["429 ステータス", "Retry-After ヘッダ", "5回目まで通る"]  # ⑥監査の照合点
area = "auth"                              # specguard.toml の area name

[[spec.requirement]]
id = "R2"
statement = "成功でカウンタをリセットする"
acceptance = ["連続失敗4回→成功→また5回試せる"]
area = "auth"
```

- **acceptance criteria が D1 監査の逐語照合点**になる。曖昧な要望をここで「反証可能な
  受け入れ条件」に落とすのが ②normalize の仕事(最も難しい部分)。
- requirement 1 つ ≒ 実装 task 1 つ ≒ 並列 agent 1 つ。粒度はここで決まる。

---

## 4. 段階間の契約(specguard 互換)

すべての agent 段は specguard と同じ「人間可読本文 + 末尾 machine marker」契約で出力する。
specguard は `<<<SPEC_AUDIT>>>` / `needs_user` / `summary`(`src/parse.rs`)。specforge は段ごとに
marker を分けるが **構造は同一**(末尾トレーラ、最後の marker が勝つ、欠落なら baseline 前進せず):

| 段 | marker | トレーラ | 欠落時 (specguard と同じ規律) |
|---|---|---|---|
| ②normalize | `<<<SPEC_DRAFT>>>` | `spec_path:` / `needs_user:` | EXIT_NO_MARKER 相当、昇格しない |
| ④prompt | `<<<SPEC_PROMPT>>>` | `prompt_path:` / `task_count:` | 同上 |
| ⑤impl | `<<<SPEC_IMPL>>>` | `branch:` / `task_id:` / `status:` | パッチ破棄 |
| ③⑥ | **specguard 既存**を呼ぶだけ | `<<<SPEC_AUDIT>>>` | specguard が処理 |

**exit code も specguard の予約と衝突させない**(`src/main.rs` の `EXIT_*`):
`0` OK / `2` usage / `3` no-marker / `4` agent-failed / `5` unratified。specforge は
これに `6`(spec 未昇格で impl 拒否)等を追加するが、既存の意味は不変。

---

## 5. 正典昇格ゲート(HOTL) — なぜ必須か

「生成された詳細仕様」がそのまま正典になると、specguard が監査する基準を機械が自分で
書いたことになり、「番人を誰が見張る」の無限後退に陥る。specguard は *prompt* に対し既に
これを解いている(`accept-prompt` で人間の同意が権威を与える、`src/ratify.rs`)。

specforge は **同じ儀式を spec に適用**する:

```
draft spec ──③specguard D2 で機械チェック(沈黙/矛盾/重複が無い)──▶ 人間レビュー ──▶ `specforge ratify <id>`
                                                                                        │
                                  status: draft → ratified, fingerprint + canon commit を lock に pin
```

- 機械(D2 監査)は **契約違反**(矛盾・沈黙)だけ弾く。**良し悪し(政策)**の判断は人間が ratify で負う。
  これは specguard の `accept_prompt` が「契約=必須 placeholder は機械、政策=理由は人間」と分けているのと同型。
- **ratified spec だけが ④prompt 生成・⑤実装の入力になれる**(未昇格なら exit 6 で拒否)。

### 5.4 ratify の出口 — requirement を backlog に積む

`ratify` は spec を昇格させた直後、**requirement 1 件につき backlog item を 1 件起票**する
(`src/forge/queue.rs`)。これが無いあいだ、ratified spec には**キューへの経路が実装として
存在せず**、「人間が `backlog add` を手で叩く」という散文だけが handoff だった。specforge が
source(課題の供給側)であるためには、合意の完了がそのまま実行キューへの投入でなければならない。

- **item の中身**: title は `[<spec id>:<req id>] <statement>`、notes には statement に加えて
  **acceptance を逐語で** `done_criteria` ブロックとして載せる。condukt の interpreter が
  statement から再導出せずに済ませるため — 再導出は requirement が黙って牙を失う場所である。
- **冪等性**: 各 item に `req:<spec id>:<req id>` という identity tag を打ち、起票前に
  `backlog list --json` の tag 集合と突き合わせる。tag は **2 つの id だけ**から導かれるので、
  statement を書き換えても item は重複しない。spec id で namespace してあるので、別 spec の
  同名 requirement(どの spec にもある `R0`)とは衝突しない。
  `--status` で絞らないのは意図的で、`done` の item も listing に残ること(実測 2026-08-02)を
  利用して**完了した requirement が再昇格で蘇らない**ようにしている。
- **判定不能は起票しない**(CLAUDE.md §3): `backlog` が無い / 非ゼロ終了 / listing が
  parse 不能 のいずれでも、「キューは空」とも「既に積んである」とも決めつけず **exit 9
  (`EXIT_QUEUE_FAILED`)** で失敗させる。前者は重複を生み、後者は人間が合意した requirement を
  黙って捨てる。**批准そのものは既に spec ファイルに書かれているので失われない**ため、
  キューが復帰したら `specforge queue --id <id>` で起票だけをやり直せる。
  これは散文の約束ではなく**型で強制**されている: subprocess は
  `harness_core::boundary::run` を通り、stdout は `CommandOutput::stdout_on_success`
  からしか取り出せない。この accessor は「どの終了コードなら走り切ったとみなすか」を
  引数で要求するので、「終了コードを見忘れた」が 1 文字の脱落として書けない。
  `enqueue_with` の戻り値も `Determination<Enqueued>` であり、`unwrap_or` も `ok` も
  `Default` も存在しない(`require` が唯一の取り出し口)。
- `specforge queue` は未批准 spec を **exit 6** で拒否する(キューは合意の下流にある)。
  起票せずに批准だけしたい場合は `ratify --no-queue` と明示する — 黙って積まないのではなく、
  積まなかったことを出力に残す。

### 5.1 正典の権威階層と矛盾エスカレーション

正典は1枚岩ではなく **権威の強さに階層がある**:

```
User 正典(ユーザが直接書いた/承認した canon)   ← 最も強い
  > ratified spec(人間が ratify 済みの生成仕様)
    > draft spec / 生成ドキュメント               ← 最も弱い
```

**重要: この階層は「矛盾の自動解決」には使わない。** 上位が下位と食い違っても、機械が
「上位が勝つ」と勝手に下位を潰してはならない。理由:

- 矛盾は **上位(User 正典)が陳腐化している兆候**かもしれない(生成側が新事実を掘り当てた)。
- あるいは生成側のバグかもしれない。**どちらかは機械には判定できない。**

ゆえに **矛盾を検出したら常に人間に Ask する**(HOTL escalation)。階層は「人間が後で
解決するときの既定/tiebreak の助言」として提示はするが、自動適用はしない。

- **異なる権威レベル間の矛盾**(例: 生成 spec が User 正典と食い違う)→ Ask。
- **同レベル文書同士の矛盾**(例: User 正典 doc A と doc B が食い違う)→ Ask。
- これは specguard の D2 で **矛盾(沈黙/重複含む)→ `needs_user: yes` → sentinel → 人間が
  `ack` するまで baseline 据え置き**、という既存規律と完全に一致する。specforge は
  この規律を **生成段(②④)にも前倒し**する: 矛盾を含む draft は ③D2 を通らず、自動昇格しない。

```
矛盾検出 ──▶ 機械は「どちらが正か」を決めない ──▶ 階層を tiebreak 助言として添えて人間に Ask
                                                       │
                                          人間が解決(どちらを直すか)──▶ ratify ──▶ 昇格
```

### 5.2 不足エスカレーション — 生成を目的化しない

specforge の成功は「draft を出すこと」ではなく「**厳格に検証可能な spec を出すか、出せない
理由を正直に上げること**」。②normalize が要望から **反証可能な acceptance criteria を導けない**
とき(情報不足・前提が曖昧・矛盾)、draft をでっち上げてはならない(hallucination は specguard が
引用できないものを `不明` に降格するのと同じ規律 — 断定しない)。代わりに:

1. **人間に質問する** — 何が決まれば厳格化できるかを具体的に問う(AskUserQuestion 相当)。
2. **不足文書をリクエストする** — 「この受け入れ条件を固めるには X という doc が要る」と明示し、
   **その doc のフォーマット/雛形まで出力してよい**(人間が埋める型を渡す。例: Spec IR の
   requirement スケルトン、非機能要件の表テンプレ、用語集の空欄表)。

出力契約では、不足時は draft ではなく **`<<<SPEC_DRAFT>>>` + `needs_user: yes` + リクエスト本文**
を返す(§4)。`spec_path:` は空。これにより未昇格のまま人間に回り、④prompt・⑤実装には進まない。

```
要望 ──▶ ②normalize ──┬─ 厳格化できる ──▶ draft spec ──▶ ③D2 ──▶ ratify
                       └─ できない ──▶ needs_user + 質問 / 不足文書リクエスト(雛形付き) ──▶ 人間
```

不足(§5.2)と矛盾(§5.1)は HOTL の **2 大エスカレーショントリガ**。どちらも「機械が無理に
決めない」点で同根 — 生成側でも specguard の `不明` / `矛盾→needs_user` 規律を貫く。**「生成
できた量」ではなく「厳格化できた割合」で評価する。**

### 5.3 厳格生成の根拠基準(rigor gate)— specguard 監査の流用

「厳格に生成できる根拠があるか」を **客観基準**にする。突き詰めると *acceptance criteria を
canon に接地できるか*。これは specguard が既に持つ判定語彙で表せ、新規発明は最小で済む:

| ゲート | 判定 | 流用元 | 落ちたとき |
|---|---|---|---|
| **G1 接地** | 各 acceptance candidate が canon に**逐語引用**で裏付く | D1 の核心(引用できねば `不明`) | §5.2 不足 |
| **G2 沈黙ゼロ** | acceptance を固める決定点で canon が沈黙していない | **D2 の沈黙検出** | §5.2 不足(doc リクエスト) |
| **G3 矛盾ゼロ** | 要望・canon 間に矛盾がない | **D2 の矛盾検出** | §5.1(どちらが正か Ask) |
| **G4 反証可能** | 各 criterion が観測可能な pass/fail | ← specforge 固有の新規判定 | §5.2 不足 |

**G1–G3 は既存 `templates/audit-prompt.md` の判定そのまま。** specguard が「実装↔canon の接地」を
見るのと同じ厳しさで「要望↔canon の接地(= spec 化できるか)」を見る。G4 だけが生成側の追加。
判定(canon が沈黙か/矛盾か)は LLM、ゲート集約は決定的ハーネス — 分担は §7 のまま。

**流用には適用タイミングを1つ足す:**

1. **Pre-flight(入力十分性監査)= 新規の適用点** — ②normalize の **前**に、要望 + その canon
   ポインタへ D2 を回し G1–G3 を判定。1つでも欠ければ `needs_user: yes` → escalate。**ここで
   「厳格生成の根拠があるか」が決まり、生成コストを払う前に弾く。**
2. **Post-draft(出力監査)= §3/§5 で既述** — 生成された draft へ D2。見かけ完全でも内部に
   沈黙/矛盾がないかを捕捉。

```
要望 + canon ──▶ [Pre-flight D2: G1–G3] ──┬─ 全通過 ──▶ ②normalize ──▶ [Post-draft D2 + G4] ──▶ ③ratify
                                           └─ 欠落 ──▶ needs_user(§5.1/§5.2)
```

**ratify(prompt 監査)も流用:** ②normalize / ④prompt-build の prompt 自体が「何を厳格とみなすか」
= メタ正典。specguard の ratify ゲート(`src/ratify.rs`)にそのまま乗せ、rigor の基準を変えたら
再批准を強制する。番人の基準を機械が勝手に緩められない。

**安全側バイアスの継承:** specguard の「逐語引用できないものは `不明`」が、そのまま rigor gate の
安全弁になる — 根拠が引けないなら「生成できる」ではなく「**生成できない**」に倒れる。過剰生成より
過小生成に倒れるので原則6(生成を目的化しない)と自動的に整合する。

---

## 6. 並列実装の隔離(⑤)

specguard の shard 並列は **read-only ゆえ隔離不要**(`src/agent.rs`, MAX_PARALLEL=4, fresh context)。
specforge の ⑤ は **複数 agent が同時に書く**ので、その前提が崩れる。対策:

1. **git worktree で物理隔離** — task ごとに `git worktree add` した別作業ツリーで agent を起動。
   ファイル競合が原理的に起きない。Claude Code の `isolation: "worktree"` と同じ発想。
2. **task は requirement 単位**(§3)。area 境界で分けるので、別 area を触る task は衝突しにくい。
3. **merge は逐次・監査付き** — 各 worktree のパッチを、⑥specguard D1 が「その task の
   acceptance criteria を満たし、他 area を壊してない」と判定して初めて merge。drift があれば
   その task だけ ⑤ に差し戻し(他の収束済み task は触らない)。

```
ratified spec ──┬─ task R1 ─▶ worktree-R1 ─▶ impl agent ─▶ ⑥D1 ─ pass ─▶ merge
                ├─ task R2 ─▶ worktree-R2 ─▶ impl agent ─▶ ⑥D1 ─ fail ─▶ 差し戻し(R2のみ)
                └─ task R3 ─▶ worktree-R3 ─▶ impl agent ─▶ ⑥D1 ─ pass ─▶ merge
```

並列度は specguard と同じく有界(既定 4)。worktree は終了時に未変更なら自動破棄。

### 6.1 受け入れ証拠と合意ゲート(⑥→⑦)= パイプラインの出口

merge の可否は機械の D1 判定だけでは決めない。**最終合意は人間**(specguard が findings を出すが
`ack` は人間、と同型)。機械は *証拠* を揃え、人間が *合意* する。

**証拠は成果物の種類で出し分ける(artifact-typed evidence)** — 「テストがある」だけにしない:

| 成果物 | 見せる証拠 | 流用元 |
|---|---|---|
| ロジック / backend | 生成テスト + **実行結果**(pass/fail) | G4 反証可能性の *実行* 版。read-only を超えるので別チャネル(§9-2) |
| **UI を持つもの** | **実際にレンダリングして見せる**(screenshot / 録画) | Claude Code の `run` / `verify` パターン |
| spec↔impl 整合 | D1 drift 監査(逐語) | specguard 既存 ⑥ |

各 acceptance criterion(§3 の G4)に証拠を 1:1 で紐付ける。**UI は文章で「動きました」と言わず、
レンダリングして見せる。** これが「やったことの証明」になる。

**変更サマリ(なにをしたか):** どの requirement を満たし、どのファイルを触り、各 acceptance を
満たす証拠はどれか、を1枚に統合して人間へ提示する。

**合意ゲート(2 分岐):**

- **合意** → merge + baseline 前進 + `specguard decide` で *理由* を canon commit に pin(⑦)。
- **相違(違う)** → 人間が **「期待と何が違うか」を入力**する。これを構造化して routing:
  - acceptance が誤り / spec の沈黙だった → Spec IR を更新(§3)→ **再 ratify 必須**(§5)。
  - spec は正しく impl が誤り → その task だけ ⑤ へ差し戻し(§6)。
  - 相違そのものを decision(D3 の driver=反証可能な理由)として記録 → 次回の判断材料。

これは §5.1 の「矛盾→人間が解決」を **受け入れ時刻**に適用したもの。期待(人間)vs 実際(生成)の
食い違いを、人間が権威として裁き、その入力がループに戻る。

```
impl ──▶ [証拠: test実行 / UIレンダ / D1逐語] ──▶ 変更サマリ ──▶ 人間に提示
                                                              ├─ 合意 ──▶ merge + decide(⑦)
                                                              └─ 相違 ──▶ 期待値を入力 ──▶ §3更新(再ratify) / §6差し戻し
```

---

## 7. 決定的ハーネス vs LLM 判定の分担

specguard の分担表(判定=LLM、それ以外=ハーネス)を specforge にも厳守する:

| 仕事 | 担当 | 理由 |
|---|---|---|
| 要望の収集・正規化先決定 | ハーネス | 入力経路は決定的に |
| 要望→受け入れ条件への翻訳 | **LLM(②)** | 自然言語理解 |
| 仕様の矛盾/沈黙検出 | **LLM(③=specguard D2)** | 既存資産 |
| ratify ゲートの fingerprint 照合 | ハーネス | 決定的・改竄検知 |
| prompt 描画 | ハーネス | テンプレ + ポインタ注入 |
| 実装コード生成 | **LLM(⑤)** | 本質的に生成 |
| worktree 隔離・merge 順序 | ハーネス | 競合制御は決定的に |
| drift 判定 | **LLM(⑥=specguard D1)** | 既存資産 |
| baseline 前進・sentinel | ハーネス(specguard) | 既存契約 |

---

## 8. 実装方式と段階

> **実装状況(2026-06-17):** 入口スライス = **②normalize + §5.3 rigor pre-flight ゲート**を
> Rust バイナリ `specforge`(specguard と同居・別 bin)として実装。`specforge draft`
> (rigor 達成→`specs/<id>.toml` draft / 未達→sentinel で HOTL escalation、でっち上げない)、
> `specforge ratify`(人間合意で draft→ratified、canon commit に pin)、`ack`。ハーネスは
> agent の `rigor:pass` 過大主張を決定的契約チェックで棄却する(§5.3 の安全弁)。fake-agent の
> 統合テストで green(実 LLM 不要)。`src/forge/`, `templates/normalize-prompt.md`,
> `specforge.example.toml`, `tests/forge_integration.rs`。
>
> **実装状況(2026-06-23):** ④⑤⑥⑦ を実装し、生成パイプラインが端から端まで閉じた。
> `specforge impl-prompt`(④: ratified spec から requirement ごとの impl prompt を生成、
> `templates/impl-prompt.md`)、`specforge implement`(⑤: requirement ごとに `git worktree`
> を切り、書き込み可エージェントを **MAX_PARALLEL=4** で並列起動、`<<<SPEC_IMPL>>>` トレーラ
> を解析して `specs/impl/<id>-impl.json` + evidence へ集約)、`specforge evidence`/`agree`
> (⑥: 成果物別 typed evidence ゲート + 人間合意)、`specforge merge`(⑦: 合意後に done の
> worktree だけを `git merge --no-ff`、失敗は per-item で握り潰しクラッシュしない)。HOTL ガード
> は決定的に強制 — 未 ratified は exit 6 で実装拒否、批准後に fingerprint が変われば再批准要求、
> 合意前 merge は gate closed で拒否。`src/forge/{impl_prompt,implement,evidence}.rs`。
> **③⑥ の機械監査(D2/D1)は引き続き specguard 本体を呼ぶ前提**(無改造で継承)。
>
> **段階C PoC(2026-06-18):** `examples/poc-loop/` に、②③(specforge)+ ⑥(specguard)を
> バイナリのまま繋ぎ、④⑤ ギャップを書き込み可エージェントで埋めるドライバ(`run-poc.sh`)を
> 追加。実エージェントで **要望→draft→ratify→impl→監査が 1 本で閉じることを実証**(仮説1: 反証
> 可能 acceptance への正規化 / 仮説2: D1 が task 単位で drift 判定 → 収束)。`run-poc.sh --drift`
> で **recovery パス**(誤実装を seed → ⑥ が B 分類で正確に検出 → 差し戻し発火)も実エージェントで
> 確認(続く ⑤fix→再収束 tail は外部 spend limit で未完、ハーネス起因ではない)。仮説3(worktree
> 並列)は `examples/poc-parallel/`(2 互いに素エリア clamp/slug)で **実エージェント実証済み**:
> 2 worktree 並列 impl → 各 ⑥ 監査クリア → 衝突なく逐次 merge → merged tree も収束。途中で
> slug の canon 沈黙により §5.2 escalation が発火(canon 厳格化→再 run で解消)も実証。PoC が
> 実バグを 1 件発見→修正: normalize エージェントが TOML の前にプロローグを置くため、
> `ir::extract_requirement_toml` で **フェンス内 TOML を抽出**する方式に変更(normalize
> プロンプトもフェンス必須に)。**段階C の3仮説が揃ったので、安定部分(④prompt-build)を段階B で
> Rust 化できる。**

**推奨: C → B の段階導入。** いきなり specguard 本体を拡張(A)すると read-only の売りが濁る。

- **段階 C(まず PoC)**: Rust を増やさず、Claude Code の **Workflow/Agent でオーケストレーション**。
  生成は agent、監査は `specguard run` を shell で呼ぶ。要望→…→再監査が **1本通って閉じるか**を実証。
  - 検証する仮説: 「②normalize が要望を反証可能な acceptance に落とせるか」「⑥D1 が task 単位で
    drift を正しく弾くか」「worktree 並列が merge 衝突なく回るか」。
- **段階 B(固める)**: 安定した部分だけ Rust 別バイナリ `specforge` に。specguard は無改造で
  検証ゲートとして呼ぶ。CLI 契約(§4 marker / exit code)を固定。
- **段階 A(将来・任意)**: 共有ハーネス(scope 解決・marker 解析・並列ランナ)を crate 化して
  両者で共有。ただし read-only 強制は specguard 側に閉じ込めたまま。

---

## 9. 未解決の設計判断(実装前に決める)

1. **要望の入力経路** — issue tracker / Markdown / 対話? IR への正規化トリガは手動 (`specforge draft`) か
   自動か。
2. **acceptance criteria の検証強度** — 証拠チャネルの構成は §6.1 で確定(D1逐語 + テスト実行 +
   UIレンダ)。残る判断は *実行系*: テスト/UIレンダを specforge 側でどう動かすか(read-only 枠を
   超えるので specguard とは別プロセス)。CI 連携か手元実行か。
3. **spec の再昇格** — ratified spec を後で編集したら、specguard の prompt drift 検知(`ratify::drifted`)
   と同じく **再 ratify を強制**する。粒度(spec 全体 vs requirement 単位)をどこに置くか。
4. **失敗 task の扱い** — ⑥D1 が N 回連続で fail した task をどうするか(specguard の sentinel 同様、
   `needs_user` で人間に上げる閾値)。
5. **決定ログ(⑦)の自動化** — converged 時に `specguard decide` を自動 scaffold するか、人間が書くか。
   理由(driver)は人間にしか書けない(specguard D3 の思想)ので半自動が妥当。

---

## 10. 入口・出口・HITL マップ — なぜ HOTL と言えるか

パイプライン全体を **境界(入口/出口)と中間**に分け、人間の介入点を1枚に整理する。
これが揃って初めて「制御されている」と言える。

```
                  ┌──────────────── 入口ゲート ────────────────┐
要望 ──▶ [① intake] ──▶ [§5.3 Pre-flight rigor gate: G1–G3] ──┐
                                                               │ 通過のみ生成へ
                                                               ▼
        ┌──────────── 中間(機械が自律実行 / 自己監査)────────────┐
        │ ② normalize ─ ③ D2 ─ ④ prompt ─ ⑤ 並列impl ─ ⑥ D1/test/UI │
        └───────────────────────┬──────────────────────────────┘
                                 │ 例外時のみ人間へ pull(sentinel)
                  ┌──────────────┴──────────── 出口ゲート ────────────┐
                  ▼                                                   ▼
        [③ ratify: spec昇格]                            [§6.1 合意ゲート: 受け入れ]
        人間の同意で正典化                               証拠提示→合意 / 相違入力→§3・§6戻し
```

### 入口は押さえているか

**Yes(原則)/ 一部 未確定(具体)。** 入口ゲートは **§5.3 Pre-flight rigor gate**。要望は
G1–G3(接地・沈黙ゼロ・矛盾ゼロ)を通らない限り生成に入れない。根拠が引けなければ §5.2 で
人間に返す。**ただし intake の *チャネル*(issue / Markdown / 対話)と正規化トリガは §9-1 で未確定** —
「何で受けるか」は決め切れていない。ゲートの *論理* は押さえたが、*窓口* は要設計。

### 出口は押さえているか

**Yes。** 出口は2枚: **③ ratify(spec の正典昇格)** と **§6.1 合意ゲート(実装の受け入れ)**。
どちらも人間の同意なしには通れず、相違時は期待値が入力されてループに戻る。decision log(⑦)で
*理由* を canon commit に pin し、provenance も閉じる。

### 中間の HITL は適切か

中間は **既定で機械が自律実行**し、人間は **例外時だけ** 引き込まれる(escalation):

| 介入点 | トリガ | 種別 |
|---|---|---|
| §5.3 Pre-flight | 根拠不足/矛盾 | 入口(同期的に止める) |
| §5.1 矛盾 / §5.2 不足 | 生成中の沈黙・矛盾 | 中間 escalation(pull) |
| prompt ratify | rigor 基準(メタ正典)の変更 | 中間(基準変更時のみ) |
| ③ ratify | spec 昇格 | 出口(同期) |
| §6.1 合意 | 実装受け入れ | 出口(同期) |
| §9-4 失敗 task | D1 が N 回連続 fail | 中間 escalation(閾値) |

**過不足の評価:** 人間ゲートは **境界(入口・出口)に厚く、中間は薄く(例外時のみ)**。これが
意図。中間の各ステップに同期承認を挟むと並列 agent がスケールせず、HOTL でなく HITL に退化する。

### それで HOTL と言えるか — 「ゲートがあるから」ではない

**核心: 人間ゲートが *存在する* ことが HOTL の根拠ではない。** HITL も人間ゲートを持つ。両者を
分けるのは **既定の制御フローと介入の様式**:

- **HITL** = 人間が **各ステップの必須ブロッキング段**。ループは毎回人間を待つ。スループットは
  人間律速。並列化が活きない。
- **HOTL** = 人間は **ループの *上*** にいて監視し、**(a) 既定では機械が自律で進み、(b) 介入は
  トリガ駆動かつ非同期(sentinel/pull)、(c) 同意ゲートは *境界* に置き中間は機械が自走する**。

specforge が HOTL である根拠は、ゲートの有無ではなく **この3条件を満たすこと**:

1. **既定は自律** — rigor 通過後の ②→⑥ は人間を待たず流れる(specguard が clean なら baseline を
   自律前進させるのと同型)。
2. **介入は非同期・例外駆動** — §5.1/§5.2 と失敗 task は `needs_user` → sentinel で *pull*。
   人間は好きなときに対応する(同期ブロックではない)。**唯一 ratify と §6.1 合意は同期的だが、
   それは *境界* に限定**(毎ステップではない)。
3. **境界に厚く中間に薄い** — 上表のとおり。

ゆえに「途中に HITL を入れたから HOTL」ではなく、**「中間を機械に自走させ、人間を境界と例外に
退けたから HOTL」**。逆に中間に同期 HITL を増やすほど HITL に寄り、並列スケールを失う ── この
トレードオフを設計の不変条件として持つ。

---

## 11. まとめ

- **実装可能**。後半(③⑥⑦)と並列・marker・ratify の土台は specguard に既にある。
- 足りないのは前半(①②④⑤)= specforge。**別バイナリ**にして read-only 不変条件を守る。
- 鍵は **Spec IR の acceptance criteria**(②で曖昧な要望を反証可能条件に落とす)と
  **HOTL 昇格ゲート**(機械は契約違反だけ弾き、政策は人間が ratify で負う)。
- まず段階 C(Workflow PoC)でループが閉じることを実証し、安定部分を段階 B で Rust 化する。
- **入口=§5.3 rigor gate、出口=③ratify + §6.1 合意ゲート**で境界を押さえる。HOTL の根拠は
  「ゲートがあること」ではなく「**中間を機械に自走させ、人間を境界と例外(sentinel/pull)に退けた
  こと**」(§10)。中間に同期 HITL を足すほど並列スケールを失い HITL に退化する ── これを不変条件にする。
