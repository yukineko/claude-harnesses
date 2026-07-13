---
name: flow
description: 課題の供給（compass の次の一手 / backlog のキュー）から解決手段の実行（condukt、fugu-router がモデル選択）までを1本のループで貫く統合 driver。source→executor を束ねる「フレームワーク層」。SessionStart で開いている仕事があれば自動で提案され（承認後に起動）、手動でも `/flow` で起動できる。判定（どの source を引くか・止め時）は LLM、状態維持・ロック・モデル選択は既存バイナリ（compass/backlog/condukt/fugu-router）が担う。
argument-hint: "[任意: 直接の課題文。省略時は compass→backlog から自動でピック]"
allowed-tools: Task, AskUserQuestion, Bash(backlog:*), Bash(compass:*), Bash(condukt:*), Bash(fugu-router:*), Bash(hypothesis:*), Bash(overwatch:*), Bash(git:*), Read
---

# /flow — 統合 source→executor driver

`/flow` は **課題の供給 → 解決手段の実行** を1本のループで回す。

```
SOURCE（課題の供給）              EXECUTOR（解決手段の実行）
  compass    … 次の右サイズの一手   ─┐
  backlog    … 確定済みキュー        ├─▶  condukt（fugu-router がモデル選択）─▶ verify
  hypothesis … 計測待ちの PDO 仮説   │
  prompt     … ユーザー直の課題文   ─┘
```

> `hypothesis` は PDO discovery の出力（検証したい仮説）を実行へ繋ぐ source。**2 相**で扱う:
> ① **open** な仮説 → **RAT ゲート**（Step 3-1 の 4）を先に通す: 未テストの高リスク×弱証拠 assumption
>    （leap of faith）があれば、full build ではなく**その assumption だけを de-risk する最小実験**に落とす。
>    leap of faith が無ければ「その仮説を検証する実験」として condukt に流す（build）。完了すると condukt が
>    gate PASS 時に `awaiting-measurement`（出荷済み・未計測）へ遷移させる。
> ② **awaiting-measurement** な仮説 → **measure step**（Step 3-1 の 2）で観測値を回収し、
>    **計測した証拠を添えて** validate/reject して閉じる（出荷だけでは validate しない＝build ≠ validate）。

**役割分担（外さない）**: ループ制御（どの source を引くか・実行・検証・止め時の判定）は **この skill（LLM）**。
状態維持・ロック・size routing・モデル選択は **既存バイナリ**（`compass` / `backlog` / `condukt` / `fugu-router`）。
この skill は新しい状態を持たず、**既存の決定論レイヤを束ねるだけ**。

## compass ／ flow ／ scout — どれから始めるか

三つの統合 source/driver があります。以下の表で状況に応じて選んでください：

| **状況** | **使うSKILL** | **役割** |
|---------|-------------|--------|
| **ゴール（北極星）が不明・矛盾・陳腐化している** | `/compass` | 再オリエンテーション → charter を彫り直す → gap と右サイズの一手を出す |
| **課題が決まっており、それを今すぐ実行・処理し続けたい** | `/flow` | source（compass/backlog/hypothesis）から課題を自動ピック → condukt へ流す → 検証→書き戻す（統合ドライバー） |
| **「今のプロジェクトに何が足りない？」を広く洗い出したい** | `/scout` | 5つのレンズ（課題・セキュリティ・業界標準・不足施策・堅牢性）で多角監査 → 施策を生成 → backlog に積む |

**進め方の典型例**:
- 最初は `/compass` で north_star を彫る → `/flow` で自動実行 → 並行で `/scout` で施策を洗い出す
- または `/scout` で施策を見つけて backlog に積む → `/flow` で順序に従って処理
- 「次に何をすればいいか分からない」なら → `/compass pivot-check` で方向判断

## いつ使うか（/flow について）

- SessionStart で「開いている仕事がある」と提案され、ユーザーが承認したとき（L2: propose-then-confirm）。
- 「次の課題を自分で選んで実行し続けてほしい」とき（手動 `/flow`）。
- `$ARGUMENTS` に課題文を直接渡せば、source 選択を飛ばしてその課題を condukt に流す。

## 競合しない理由（重要）

- **source と executor は直交**し、state ディレクトリも別（compass / backlog / condukt はそれぞれ独立ストア）。
- `/flow` は **backlog のロックを共有**して直列化する。`/backlog` と同時に走らせない（両方が condukt run を生むため）。**`/flow` は `/backlog` の上位互換**（compass ゲート＋複数 source を足したもの）。
- compass は **ゲート兼優先順位付け**、backlog は **確定キュー**、condukt は **executor** という分担を崩さない。

---

## 手順

### Step 0 — 引数分岐

`$ARGUMENTS` に課題文があれば → **Step 3（その課題文で condukt 実行）へ直行**。ループはせず1件だけ実行して終了（明示課題は「今これをやれ」の意味）。
引数が空なら → Step 1 へ（source から自動ピックするループ）。

### Step 0.5 — 自律ゲート（`condukt policy answer` で per-gate graded 判定）

ループ中に人間へ問い合わせる（`AskUserQuestion`）箇所は、**自律モードでは各ゲート固有の
risk×reversibility×confidence を `condukt policy answer` に渡し、決定論的 verdict に従って
自答／エスカレート／拒否する**（グローバル一括の縮退ではなく**ゲート単位**の graded 判定。
condukt / scout と**同一**の shim。5f7d706b で出荷済み）。

まず**グローバルな自律スイッチ**で「そもそも縮退してよいか」を確認する（非自律の既定は従来どおり全 Ask）:

```bash
condukt state autonomy-check   # exit 0 + {"autonomous":true} → 自律 / exit 1 + {"autonomous":false} → 非自律
```

- **exit 1（非自律・既定）** → **従来どおり全ゲートで `AskUserQuestion`**（後方互換。挙動を一切変えない）。
- `autonomy-check` が存在しない版（`exit 127` / "unknown subcommand"）→ **非自律とみなす**（安全側フォールバック＝全 Ask）。
- **exit 0（autonomous）** → 各ゲートを次の **policy-answer routing** に通す（縮退の既定を hardcode せず、verdict で決める）:

```bash
# 各 human gate はまずこの shim を通す。exit code で 自答 / 従来 Ask / 拒否 を分岐する。
OUT=$(condukt policy answer \
        --risk <low|medium|high> --reversible <low|medium|high> --confidence <low|medium|high> \
        --question "<質問文>" --option "<A>" --option "<B>" --recommend <既定 index> 2>/dev/null)
case $? in
  0) CHOSEN=$(printf '%s' "$OUT" | jq -r '.chosen') ;;  # auto: 自答。CHOSEN を採用し Ask しない（自答は監査ログに追記される）
  2) : ;;  # escalate: 従来どおり AskUserQuestion（＝残す唯一の 質疑 channel）
  3) : ;;  # block: 実行を拒否して停止（人にも聞かない hard stop）
  *) : ;;  # 1(不正入力)/127/旧バイナリ（`answer` 無しの clap exit 2 も case 2 に落ちて安全）→ 安全側 = AskUserQuestion
esac
```

- **exit 0（auto）** → stdout（`{"answered":true,"policy":"auto","chosen":"..","recommend_index":N}`）の `chosen` を採用し、**Ask しない**。
  この自答は `gate-decisions.jsonl` に追記され、**`condukt policy answers` で後から監査できる**（撤去したゲートの review surface）。
- **exit 2（escalate）** → **従来どおり `AskUserQuestion`**。旧バイナリが `answer` を持たない場合の clap `exit 2` もここに落ちる＝**フェイルセーフ**。
- **exit 3（block）** → 実行を拒否して停止する。
- **その他（exit 1 不正入力 / exit 127）** → 安全側にフォールバックして `AskUserQuestion` を出す（never break a turn）。

各ゲートに与える risk/reversibility/confidence と既定（`--recommend`）:

| human gate | risk | reversible | confidence | 典型 verdict | 自答時の既定（recommend） |
|---|---|---|---|---|---|
| **ロック競合**（Step 2・生きた保有者） | low | high | high | auto | **stand down**（報告して clean exit。`--force` 自動奪取はしない） |
| **resume 選択**（複数候補） | low | high | high | auto | 3-1 の優先度 pick 規則の先頭 |
| **pivot-check**（Step 4・`pivot`） | medium | high | low | **escalate** | —（genuine な戦略判断なので人に聞く。既定案＝継続/persevere） |
| **循環ブレーカー trip**（早期脱出・`condukt circuit check`） | low | high | high | auto | **clean stop**（ループを止め Step 4 へ） |

> pivot は **escalate（残す 質疑）**＝ streak 閾値超えは「戦略が効いていない」という genuine な判断材料なので人間に返す。
> それ以外の routine なゲートは **auto** で自答され Yes/No は消える。verdict は `policy::decide`
> （`risk − reversible − confidence` の決定論スコア: `≤ -2`→auto / `≥ 1`→block / それ以外→escalate。
> ただし risk=high かつ reversible=low は無条件 block）が確定するので、ここで挙動を hardcode しない。

**安全不変条件（自律でも残す停止）**: 自律モードで残る human stop は **(a) worker が blocked**
（condukt がエスカレーション）、**(b) deploy/push の GATED 承認**、**(c) pivot**（上表で escalate に倒す
genuine な戦略判断）、および **policy answer が escalate/block を返したゲート**。それ以外の routine な human gate は
policy-answer の auto で自答され Yes/No は消える（監査ログに残る）。**budgetguard の予算超過による早期脱出（Step 4）は
どのモードでも維持**する。

### Step 1 — compass ゲート（盲目実行の防止）

source を引く前に、ゴールが鮮明かを確認する:

```bash
compass gap     # ゴール−現状の gap と候補の一手を出す
```

- charter が **陳腐・矛盾・抽象すぎて一手が引けない**場合 → **自動実行しない**。
  ユーザーに「先に `/compass` で再オリエンテーションが必要」と伝えて**停止**する（権威で自動解決しない）。
- charter が鮮明で **右サイズの一手が引ける**場合 → その一手を `to_condukt` 候補として保持し、Step 2 へ。

> compass は「ONE に絞り残りは parked」が思想。`/flow` はそれを尊重し、compass の主筋を**最優先 source** として扱う。

### Step 2 — ロック取得（クロスセッション直列化）

backlog のロックを使って二重ループを防ぐ:

```bash
backlog lock status
backlog lock acquire --session-id <SESSION_ID> --project <CWD>
```

- 別セッションがアクティブにロック保持中（**生きている保有者**）→ **Step 0.5 の policy-answer routing** に通す
  （`--risk low --reversible high --confidence high`、`--question "生きた保有者がロック中。どうする?"`、
  `--option "stand down" --option "wait" --option "force-steal" --recommend 0` → 既定 verdict は auto）:
  - **auto（exit 0）** → `chosen`（＝stand down）を採用: `--force` の自動奪取はせず、「別セッションが実行中のため見送り」と
    報告して**clean exit**（ロック未取得のまま正常終了。生きた保有者は決して奪わない）。自答は監査ログに残る。
  - **escalate（exit 2）／ 非自律・旧バイナリ・不正入力のフォールバック** → 従来どおり
    `AskUserQuestion`（待機 / 強制奪取 `--force` / 中止）。`--force` は **生きている保有者からも奪取**する
    （`backlog lock acquire --force ...`）。
- stale（保有 pid が死亡）なら `acquire` が**自動で reap** するため `--force` は不要（自律・非自律とも同じ既存挙動）。
- 取得失敗時は理由を報告して終了。

### Step 3 — 実行ループ（繰り返し）

「source が尽きる / 予算超過 / ユーザー中断」まで以下を繰り返す。

#### 3-1. 次のタスクを優先度順にピック

1. **compass の主筋**（Step 1 の `to_condukt`）が未消化なら → それを最優先で選ぶ。
2. **measure step（計測ループを閉じる / build ≠ validate）** — 新規 build より**先に**、出荷済み・未計測の仮説を回収する:
   ```bash
   hypothesis list --status awaiting-measurement   # condukt が merge 時に遷移させた「出荷済み・未計測」
   ```
   - 各 awaiting-measurement 仮説について、**計測信号が今観測可能か**を判定する:
     - **観測可能** → これは **condukt build ではなく measure タスク**。実験で観測した成果を集め、
       そのまま 3-3 の sink で `hypothesis validate/reject --evidence` して**仮説を閉じる**
       （この 1 件はここで完了。condukt は起動しない）。3-2 を飛ばして 3-3（measure 由来）へ。
     - **まだ観測不能**（データ蓄積待ち等）→ awaiting-measurement のまま残し、
       「計測待ち（まだ観測不能）」として報告し次の候補へ進む（ここで無限ループしない）。
   - `hypothesis` バイナリが無い / 0 件なら skip。
3. measure 対象（今観測可能なもの）が無ければ **backlog**（確定キュー）。
   backlog に **複数の ready 課題がある場合は、順列（1件ずつ）ではなく 1 回の condukt run に束ねて並列処理**する。
   **並列/直列の判定は flow がするのではなく、condukt の決定論スケジューラ（`schedule.rs`）に委譲**する
   ＝ flow は独立候補を「束ねて渡すだけ」で、ファイル競合・`Serial`/`Gated` クラス・shared-glob・依存層は
   condukt が判定し、非衝突タスクだけを並列バッチに、衝突・危険なものは自動で直列に落とす
   （**「並列が危険/高コストなら直列」はこの層で保証**される＝conservative: 迷えば直列）。

   a. **バッチを取り出す**（優先度順の上位 N 件。並びは priority→weight 降順→created_at）:
      ```bash
      backlog list --status pending --project "$PWD" --json   # 並び順どおりの JSON 配列
      ```
      先頭から最大 **N 件**（既定 N=condukt の `max_parallel`。無指定なら **4**）を候補バッチとする。
      各件の `id` / `title` / `notes` / **`hashkey`** を控える（sink で **id ごとに** `done`/`fail` する、
      および claim の解放に必須）。backlog が 1 件だけなら従来どおり単一課題として扱う（N=1）。
      **claim-skip ゲート（多重着手の防止）**: 候補バッチの各 item について
      `condukt state is-claimed --hashkey <hashkey>` を実行する。**exit 0（他セッションが既に claim 中）
      → その item はスキップして次候補へ**（この機能の主目的＝クロスセッションでの二重着手防止）。
      `condukt` が無い/失敗した場合は fail-soft（従来どおりピックを続行）。
   b. **コスト/危険ゲート（直列フォールバック）** — 次のどれかに該当する候補は**バッチから外して 1 件ずつ直列**に回す（安全側）:
      - budgetguard が予算逼迫を示す → バッチ幅を絞る（極端なら N=1＝従来の直列に縮退）。
      - notes から明らかに **相互依存**／同一領域・同一ファイルを触る／deploy・push（Gated 相当）を含むと読める。
      判断に迷うものは**バッチに入れてよい**（condukt が衝突を検出して自動で直列化するため二重の安全網になる）。
   c. **backlog に積む側**（このループが `backlog add` する場合）— compass opportunity 由来なら **その weight を供給**する
      こと（weight が compass→backlog→flow と流れ、影響度の高い機会が先頭に来て同じ並列バッチに乗りやすくなる）:
      ```bash
      W=$(compass gap | jq -r '.opportunities[0].weight // empty')   # active outcome の最重要 opportunity の weight
      backlog add --title "<課題>" --project "$PWD" --priority p1 --weight "${W:-0}"
      ```
      weight 無指定は既定 0.0＝従来の (priority, created_at) 順（後方互換）。weight は順序を変えるだけで priority は上書きしない。
4. backlog も空なら **hypothesis（新規 discovery: open 仮説）**:
   ```bash
   hypothesis list --status open    # confidence 降順（同点 created_at 昇順）でソート済み。空なら次へ
   ```
   **`list --status open` は confidence 降順で並ぶ**ので、**先頭（最高 confidence ＝ 最も検証価値が高い仮説）から順にピック**する
   （挿入順ではなくスコア順で discovery を駆動する。各行頭の `(conf X.XX)` が検証優先度）。
   open な仮説があれば、**full build に直行する前に RAT ゲート（riskiest-assumption test）を通す**:
   ```bash
   RAT=$(hypothesis rat <hid>)      # 未テストの最重要×弱証拠 assumption（leap of faith）を 1 行返す
   ```
   - `RAT` が**非空**（高リスク・未テストの leap of faith がある）→ 課題文は **full build ではなく、
     その assumption だけを検証する最小 de-risk 実験**にする（"<assumption text> が成り立つかを最小コストで測る実験"）。
     `RAT` 行頭の index を控え、3-3 の sink で `hypothesis tested <hid> <index>` を呼んで計測ループを閉じる。
   - `RAT` が**空**（高リスクの未テスト assumption が無い＝既に de-risk 済み）→ 従来どおり
     その**仮説を検証する実験**（full build）を課題文にする。
   いずれも仮説 ID を控える。`hypothesis` バイナリが無い / 0 件 / `rat` 未対応なら従来どおり full build に流す。
5. compass 主筋・measure（観測可能なもの）・backlog・open 仮説のいずれも**実行可能なものが無い**
   → **ループを抜けて Step 4 へ**（awaiting-measurement にまだ観測不能な仮説が残っていても、
   それは「計測待ち」として残課題に計上しループは終える）。
6. ピックしたタスクを**課題文**に組み立てる:
   - **単一課題**（compass 主筋 / measure / hypothesis / 単発 backlog）→ タイトル＋ notes（仕様・制約・参照ファイル）で従来どおり 1 課題文。
   - **backlog バッチ（3 の a/b で複数残った場合）→ 1 つの課題文に、各 backlog item を「独立した top-level タスク」として列挙**する。
     各項目に **id・タイトル・notes・（分かるなら）触るファイル/領域**を明記し、「**これらは互いに独立。非衝突なものは並列に、
     衝突・共有リソースを触るものは直列に scheduleしてよい**」と condukt に明示する（分解時に item 境界を保てるよう、
     item 単位で done_criteria を切ってもらう）。**item id ↔ condukt タスク**の対応を控える（sink で id ごとに書き戻すため）。
     並列上限は condukt の `max_parallel`（既定 4）が実効的に効くので flow 側で待ち合わせ制御はしない。
7. **overwatch に anchor を登録**（課題文を組み立てた直後、選んだ source 種別によらず必ず）— これにより
   condukt run を起こさない measure step でも「今どのセッションが何を担当しているか」が project-wide
   レジストリ（`overwatch status`）に乗る:
   ```bash
   overwatch begin --key "<pdo-unit-id>" --title "<task title>" \
     --scope "<touched_files をカンマ区切り、不明なら省略>" \
     --done-criteria "<done_criteria>"
   ```
   - `<pdo-unit-id>` は backlog なら `hashkey`、compass 主筋 / measure / hypothesis なら move / 仮説 ID など
     その PDO 単位を一意に指す文字列。**この key は Step 4 の `overwatch end` で解放するため控えておく**。
   - `--scope` は分かるなら condukt の `touched_files` と同じ語彙（カンマ区切り）で渡す。調査・carve ループ中など
     scope が未確定なら**省略**する（空 scope は衝突検知の対象外）。
   - **バッチ（複数 backlog item）なら item ごとに `overwatch begin` を呼ぶ**（key = 各 item の `hashkey`）。
   - **fail-soft**: `overwatch` バイナリが無い / 呼び出し失敗時は skip して続行する（既存の
     `backlog`/`condukt`/`compass` 欠落時と同じ方針＝turn を壊さない）。

8. **選択を shared discovery store に記録**（未選択は `discovered` で次サイクルへ。バッチなら選んだ各 item を記録）:
   ```bash
   compass discovery select --session-id "<SESSION_ID>" --title "<選んだタスクのタイトル>"
   ```
   - 失敗時は fail-soft（compass 欠如 / 呼び出し失敗時も続行）。

9. **着手前に claim する（TOCTOU の最終ガード）** — 選んだタスク（バッチなら各 item）について:
   ```bash
   condukt state claim-task --run "flow-$CLAUDE_CODE_SESSION_ID" --session "$CLAUDE_CODE_SESSION_ID" \
     --title "<title>" --hashkey <hashkey>
   ```
   **exit 1（別セッションが直前に claim 済み＝スキップされた）→ その item は諦めて 3-1 に戻り次候補へ**
   （3-1 の claim-skip ゲートと 3-2 の condukt 起動の間の隙間を塞ぐ最終ガード）。
   `condukt` が無い/失敗した場合は fail-soft（従来どおり実行を続行）。

#### 3-2. condukt で実行（fugu-router がモデル選択）

課題文を `/condukt` に渡す。condukt が分解 JSON を出したら、`fugu-router` が各タスクの `suggested_model` を実績から上書きする（併用時）:

```
/condukt <課題文>
```

- `/condukt` は **`Task` ツールで非同期起動**（オーケストレーション継続のため）。
- compass 由来の一手なら、`north_star / current_gap / measuring_stick` を文脈として課題文に添える。
- **backlog バッチは 1 回の `/condukt` 呼び出し**（複数 condukt run を並走させない＝グローバル backlog ロック /
  worktree / merge 競合を増やさない）。並列化は condukt 内部（Phase 5 の worktree 並列 + schedule.rs のバッチ）が担う。
- **verify も自動で並列化される**: condukt の Phase 6 は worker 完了ごとに即 verifier を起動し待ち合わせしない
  （pipeline 検証）。バッチで複数 item を渡せば、その検証も item 横断で並列に走る＝別途 flow 側で verify を並列化する必要はない。

#### 3-3. 検証 → sink（結果の書き戻し）

condukt の完了ゲートを通ったら結果を source に書き戻す:

**バッチ（複数 backlog item を 1 run に束ねた場合）は item ごとに個別 sink する**: condukt の完了ゲートは
タスク単位なので、6 で控えた **item id ↔ condukt タスク**の対応を使い、**通ったタスクの item は `done`、
blocked/失敗の item は `fail`** と書き分ける（**部分成功をそのまま反映**＝一部が失敗しても通った分は done にする。
バッチ全体を一括で成功/失敗扱いにしない）。以下は 1 item あたりの sink:

- **成功**:
  - backlog 由来 → `backlog done <id>`（バッチなら通った各 item の id について実行）
  - compass 由来 → 完了した move を **measuring_stick で判定**し、その verdict を記録する（＝計測ループを閉じる）:
    ```bash
    compass outcome --verdict <forward|unchanged|backward> --evidence "<観測した成果>"
    ```
    verdict は move の diff・テスト結果・gap への接近度から **driver(LLM) が判定**する（前進=forward / 不変=unchanged / 後退=backward）。
    `--evidence` は計測値（テスト数・ベンチ・観測した挙動）を必須とする＝出荷だけでは記録しない（build ≠ validate）。
    記録後 `compass gap` を取り直すと `last_outcome` が次サイクルに反映される（人手の別コマンド不要＝sink の一部として自動記録）。
  - hypothesis 由来（**新規 experiment の build が完了**）→ condukt は gate PASS 時に linked_hypotheses を
    **`awaiting-measurement`（出荷済み・未計測）へ遷移済み**。**出荷しただけでは validate しない**ので、
    flow はこの場で validate/reject せず、仮説を awaiting-measurement に残す。閉じるのは**次サイクルの
    measure step（3-1 の 2）**が観測値を添えて行う（build ≠ validate）。「計測待ち N 件」を残課題として報告する。
  - measure step 由来（**3-1 の 2 で観測値を回収した awaiting-measurement 仮説**）→ 観測した成果を添えて閉じる:
    ```bash
    hypothesis validate <id> --run <RID> --evidence "<観測した成果>"   # 反証なら reject <id> --reason "<反証内容>"
    ```
    これで awaiting-measurement → validated / rejected に遷移し、計測ループが閉じる
    （`validate`/`reject` は証拠必須なので、観測値の無い「出荷だけ」では status を変えられない）。
  - fugu-router 併用時 → 検証結果（どのモデルが通ったか・コスト）を `record` で書き戻して方策を更新。
- **失敗**（blocked / needs-serial 等）:
  - backlog 由来 → `backlog fail <id> --reason "<概要>"`、スキップして次へ（バッチなら**失敗した item だけ** fail、他は上記どおり done）。
  - ユーザーに失敗を通知するが、ループは続行。

**claim の解放（成功/失敗どちらの sink でも必須）**: `backlog done`/`backlog fail` の直後（または中断時）に、
その item の claim を解放する:
```bash
condukt state release-task --run "flow-$CLAUDE_CODE_SESSION_ID" --hashkey <hashkey>
```
他セッションが同じタスクを取れるようにするため。**バッチなら item ごとに release** する（6/a で控えた
`hashkey` を使う）。`condukt` が無い/失敗した場合は fail-soft（release できなくても TTL で自動 reap されるため
ループは続行する）。

#### 3-4. ループ継続判定

3-1 に戻る前に、保持中の claim を live に保つため定期的に heartbeat する:
```bash
condukt state heartbeat --run "flow-$CLAUDE_CODE_SESSION_ID"
```
（heartbeat が途切れた claim は TTL で自動 reap されるため、長時間のループでは各サイクルで呼ぶのが安全）。
`condukt` が無い/失敗した場合は fail-soft（従来どおりループを続行）。

続けて **決定論の循環ブレーカー**を1本のコマンドで判定する（cost・failure-streak・stall を集約。詳細は「早期脱出」）:
```bash
condukt circuit check --run "flow-$CLAUDE_CODE_SESSION_ID"   # trip なら nonzero、continue なら exit 0
```
**nonzero（trip）なら人にも policy にも聞かず即 Step 4 へ**（stop 理由 slug は JSONL に記録）。exit 0 なら 3-1 に戻る。
`condukt` が無い/失敗する版では fail-soft で従来の散文フォールバック（下記早期脱出表）に落ちる。

3-1 に戻る。早期脱出条件（下記）に当たれば Step 4 へ。

### Step 4 — ロック解放とサマリ

source が尽きた / ユーザー中断 / 予算超過のいずれかで:

```bash
backlog lock release
```

**早期脱出時もロック解放は必須**。

**overwatch anchor の解放（Step 3-1 の 7 で登録した各 anchor のライフサイクルを閉じる）**: 3-1 で
`overwatch begin` した各 `<pdo-unit-id>` について、対応する `end` を呼ぶ:
```bash
overwatch end --key "<pdo-unit-id>" --status "<done|abandoned>"
```
- `--status` は sink の結果を反映する（成功で閉じたなら `done`、失敗・未完で手放すなら `abandoned`）。
- **バッチなら item ごとに `overwatch end`**（begin と同じ key＝各 item の `hashkey` を使う）。
- **fail-soft**: `overwatch` バイナリが無い / 呼び出し失敗時は skip する（既存の backlog lock release /
  claim 解放と同じ方針＝解放できなくても TTL で自動 reap されるため turn を壊さない）。

最後に「処理件数・成功・失敗・残キュー・次に取り直した gap」を報告する。

#### pivot-check（ループ終端の方向判断）

ロック解放の直後、ループを正常終了した場合（中断・エラー以外）は以下を実行する:

```bash
compass pivot-check   # {"recommendation":"persevere"|"pivot","streak":N,"threshold":N,"reason":"…"}
```

- **`persevere`** → そのまま継続。「次の gap を取り直す」と報告する。
- **`pivot`** → **Step 0.5 の policy-answer routing** に通す（`--risk medium --reversible high --confidence low`
  ＝ streak 閾値超えは genuine な戦略判断なので既定 verdict は **escalate**。
  `--question "pivot 兆候。north_star を彫り直す?" --option "再オリエンテーション" --option "継続" --recommend 1`）:
  - **escalate（exit 2）／ 非自律・フォールバック** → `reason`（streak 長・対象 verdict 列）を引用してユーザーに提示し、
    **north_star を彫り直す（再オリエンテーション）か否か**を問う（`AskUserQuestion`）。「再オリエンテーション」なら
    `/compass` を案内して終了、「継続」なら通常どおり報告して終了。
  - **auto（exit 0）** → `chosen`（既定案＝継続/persevere）を採用: `reason` を報告に引用しつつループは止めず
    「次の gap を取り直す」で継続する（彫り直しは保留＝勝手に `/compass` しない）。自答は監査ログに残る。
  pivot 判定は `compass outcome` を積み重ねることで精度が上がるため、outcomes が 0 件なら pivot-check はスキップしてよい。

## 早期脱出

**決定論的な循環ブレーカー（cost・failure-streak・stall を1本のゲートに consolidate）**: ループの各イテレーション
（3-4 の継続判定の一部）で `condukt circuit check --run "flow-$CLAUDE_CODE_SESSION_ID"` を実行する。この 1 コマンドが
**failure-streak がキャップ（既定 3）到達・予算超過・no-progress TTL（既定 1800 秒）超過**の3条件を決定論で判定し、
どれかが成立すれば **nonzero で trip** する（成立しなければ exit 0＝continue）。trip を観測したら **人にも policy にも
聞かず即 clean stop** して Step 4 へ（停止理由 slug は JSONL に記録され後から可観測）。信号採取はすべて fail-soft
（run 未ロード・budgetguard 不在などは非 trip に縮退）で、`condukt` が無い/失敗する版では従来の下表フォールバックに落ちる。
これで「連続失敗 3 件」という散文だった停止条件が **1つの決定論ゲート**に集約される（散文が唯一の停止機構ではなくなる）。

| 状況 | 対応 |
|---|---|
| ユーザーが中断を指示 | 直ちに Step 4（ロック解放）へ |
| 循環ブレーカーが trip（下記のとおり毎イテレーション `condukt circuit check --run RID` を実行し **nonzero**＝failure-streak がキャップ到達・予算超過・no-progress stall のいずれか） | **決定論的に clean stop**（ループを止め Step 4 へ。人にも policy にも聞かない hard stop。停止理由は verdict の JSONL に記録される）。非自律で追加確認を入れたい場合の**フォールバックのみ** `AskUserQuestion`「続行 / 中止」 |
| budgetguard が予算超過を返す | ループ終了（Step 4）。残キューはそのまま次セッションへ（予算軸は上の circuit check にも consolidate 済み） |
| compass ゲートが「再スコープが必要」を示す | ループを止め、`/compass` をユーザーに促す |
| `backlog next` が予期しないエラー | 報告して Step 4 へ |

## ハードルール

- **source/executor の役割を混ぜない**: 課題の選定は compass/backlog、実行は condukt。`/flow` 自身は判定とループだけ。
- **driver は1本**: `/flow` 実行中は `/backlog` を併走させない（backlog ロックで物理的に直列化されるが、ユーザーにも明示する）。
- **並列は「バッチを 1 condukt run に束ねる」で実現**（複数 condukt run を並走させない）: backlog に複数 ready 課題が
  あれば順列ではなく 1 回の condukt run に束ねて渡し、**並列/直列の実判定は condukt の `schedule.rs`（ファイル競合・
  Serial/Gated・shared-glob・依存層）に委譲**する。flow 自身は独立候補を束ねるだけで、危険/高コストなら condukt が
  自動で直列化する（conservative＝迷えば直列）。予算逼迫や明白な相互依存が読めるときは flow 側でバッチ幅を絞る（極端は N=1）。
- **盲目実行しない**: compass ゲートが鮮明でない限り、自動でキューを流し始めない。
- **ロック解放を絶対に飛ばさない**（早期脱出・エラー時も）。
- **自律モードでは human gate を `condukt policy answer` に通す（Step 0.5）**: `autonomy-check` exit 0 のとき、
  各ゲート（ロック競合 / resume 選択 / pivot）を per-gate の risk×reversible×confidence で
  `policy answer` に掛け、**auto は自答（Ask 撤去・監査ログに追記）／ escalate は従来 Ask（残す 質疑）／ block は拒否**。
  なお **failure-streak/予算/stall の早期脱出は policy answer ではなく決定論の `condukt circuit check` に集約**されており、
  trip すれば人にも policy にも聞かず clean stop する（上記「早期脱出」）。routine なゲート（ロック競合＝stand down、
  resume＝優先 pick 先頭、循環ブレーカー trip＝clean stop）は auto で消え、
  **pivot は escalate**（genuine な戦略判断）として残る。自律で残る停止は **(a) pivot** **(b) worker blocked**
  **(c) deploy/push の GATED 承認** **(d) budgetguard 早期脱出**、および policy が escalate/block を返したゲート。
  exit 1（既定・非自律）は**従来どおり全 Ask を維持**（後方互換）。存在しない版（exit 127）は非自律とみなす。
