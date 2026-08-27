---
name: scout
description: プロジェクトを多角的に監査して「施策（タスク）」を生成する SOURCE。現在の課題・セキュリティ・業界/他プロジェクトの標準・不足施策・安全性の5レンズを sub-agent で並列調査し、逐語引用つきの施策候補を統合・重複排除・スコアリングして backlog に積み、/flow に実行を引き渡す。compass が単一ゴールの勾配なら、scout は広域偵察で複数施策を生む相補的 SOURCE。判断（監査・施策の選別）は LLM、保存は backlog、実行は flow/condukt。
argument-hint: "[任意: 監査対象パスやレンズ絞り込み。例: 'security のみ' / 'crates/condukt'。省略時は repo 全体5レンズ]"
allowed-tools: Task, AskUserQuestion, WebSearch, WebFetch, Read, Grep, Glob, Bash(git:*), Bash(cargo:*), Bash(cargo-deny:*), Bash(backlog:*), Bash(compass:*), Bash(specguard:*), Bash(condukt:*), Bash(ls:*), Bash(rg:*), Bash(grep:*)
---

# /scout — 多角監査による施策生成 SOURCE

`/scout` は **プロジェクトを5つのレンズで偵察 → 施策（タスク）を生成 → backlog に積む → /flow へ引き渡す**。

```
REVIEW（決定論的な現状収集）
   ↓
5 LENS 並列調査（read-only sub-agent）
   現在の課題 / セキュリティ / 業界標準(Web) / 不足施策 / 安全性
   ↓
SYNTHESIZE（統合・重複排除・スコアリング）
   ↓
backlog add（承認済み施策）── /flow ─▶ condukt（実行）
```

**役割分担（外さない）**: 監査と施策の選別（判断）は **この skill（LLM）+ sub-agent**。
施策の保存は **backlog バイナリ**、実行は **flow/condukt**。scout は **新しい状態を持たず**、
read-only で課題を *発見* するだけ。実装は一切しない。

## いつ使うか

- 「今のプロジェクトに何が足りない？ 次に打つべき施策は？」を**広く**洗い出したいとき。
- セキュリティ・安全性・CI・テスト・docs など**横断的な健全性**を点検したいとき。
- compass（単一ゴールの勾配＝ONE に絞る）では拾えない**複数の独立した施策**が欲しいとき。

> compass は「一手に絞り残りは parked」。scout は逆に「広く挙げて backlog に積む」。
> 両方とも source であり、最終的に backlog→/flow→condukt の同じ executor に合流する。

## 不変条件（外さない）

1. **監査は read-only** — scout も sub-agent もファイルを編集しない。施策の実行は flow/condukt に委ねる。
2. **証拠のない施策は採用しない** — 各施策は逐語引用 or `file:line` 参照 or Web ソース URL を必ず持つ（幻覚防止）。
3. **書き込みは backlog add のみ** — scout はループもロックも持たない。直列化が要る実行は /flow（backlog ロック）に任せる。
4. **合意は main のみ** — backlog に積む施策の確定は `AskUserQuestion`（HOTL）。勝手に全件積まない。

---

## 手順

### Phase 0 — スコープ受領

引数から監査スコープを取る:
- 空 → repo 全体・5レンズ全部。
- `security のみ` 等 → 指定レンズに絞る。
- `crates/condukt` 等のパス → そのサブツリーに絞る。
- `--dry-run` → Phase 3 の施策提示で止める（backlog に積まない）。

`condukt --version` でハーネスの存在を確認（無くても scout 自体は動くが、実行引き渡し先が無いと警告）。

### Phase 0.5 — スコープ規模でレンズを自動縮約（コスト・ゲート）

5レンズ全開（特に **L3 は WebSearch/WebFetch を使い高コスト**）は、狭いスコープには過剰。
Phase 0 の**明示的なレンズ絞り**（`security のみ` 等）が無いときでも、**スコープ規模に応じて
レンズ数を決定論的に縮約**する。まず対象規模を測る（read-only・失敗時は「大」とみなし全開）:

```bash
# スコープ内の追跡ファイル数（パス指定が無ければ repo 全体）
git ls-files -- "${SCOPE_PATH:-.}" | wc -l
```

ファイル数（`N`）で既定レンズを決める。`severity` 観点の強いレンズ（L1/L5）は常に残す:

| スコープ規模 | 既定レンズ | L3（業界標準・Web）の扱い |
|---|---|---|
| **小**（`N ≤ 10` / 単一ファイル〜数ファイル） | **L1 + L5**（局所の課題＋堅牢性のみ） | **省略**（Web は割に合わない） |
| **中**（`10 < N ≤ 80` / 単一 crate 規模） | **L1 + L2 + L4 + L5** | **省略**（依存・不足施策はローカルで足りる） |
| **大**（`N > 80` / 複数 crate・repo 全体） | **L1–L5 全開** | **実施**（横断スコープでこそ業界標準比較が効く） |

**追加の縮約（直近 scout 履歴）**: L3 は Web 結果の鮮度が高い。`backlog list` に直近 scout 由来の
L3 施策が既にあり、対象スコープに重なるなら、**大スコープでも L3 を省略**してよい（重複 Web コスト回避）。

**上書き規則**:
- Phase 0 の**明示レンズ絞り**が最優先（ユーザーが `L3 も` と言えば規模に関わらず実施）。
- セキュリティ懸念が事前に分かっている（依存更新・認証コード変更等）なら、小・中でも **L2 を足す**。
- 縮約した結果は Phase 3 のサマリで「規模 N=… のため L?,L? に縮約（L3 省略理由: …）」と**明示**する
  （網羅性を黙って削らない＝静かな打ち切り禁止）。

### Phase 1 — 決定論的レビュー（現状の事実収集）

施策を**事実に接地**させるため、まず決定論的にプロジェクト状態を集める（全て read-only・失敗は無視）:

```bash
git log --oneline -15                       # 直近の流れ
git status --short                           # 未コミット
cargo test --workspace 2>&1 | tail -15       # テスト健全性（Rust repo の場合）
compass gap 2>/dev/null | head -40           # 北極星ゴールとの gap・DoD・measuring stick
backlog list --status pending --project "$PWD"  # 既存の施策（重複回避の素。backlog の status は pending|done|failed）
specguard prompt --json 2>/dev/null | head   # spec-drift の有無（あれば）
cargo deny check advisories 2>&1 | tail -20  # 依存の既知脆弱性（cargo-deny があれば）
ls .deepwiki/*.md 2>/dev/null                # アーキテクチャ wiki（背景）
```

得られた要約を **REVIEW コンテキスト**として保持し、全 sub-agent に渡す（各 agent が重い再収集をしないため）。
**compass の `measuring_stick` があれば施策スコアリングの基準として最優先で採用**する。

### Phase 2 — 5レンズ調査（read-only sub-agent・**最大 3 並列の波**）

`Task` で **read-only な探索 agent**（`Explore` 等。ファイル編集権を持たない型）を起動する。
**Phase 0.5 で選ばれたレンズ分だけ**起動する（明示絞り込みも規模ゲートも無い repo 全体なら 5 体全開、
狭いスコープなら L1/L5 等に縮約）。

**同時に起動してよいのは 3 体まで**（1 セッションあたりの同時実行上限＝
`harness_core::parallel::SESSION_MAX_PARALLEL`。condukt の `max_parallel` と同じ数）。
レンズが 4 つ以上なら **3 体ずつの波**に分ける（例: 5 レンズ → L1/L2/L3 を 1 メッセージで起動 → 全部
戻ったら L4/L5）。**レンズを間引いて 3 に収めてはいけない** — 上限は波を分ける理由であって、
調査範囲を削る理由ではない（削るなら Phase 0.5 の絞り込みで、理由を明示して行う）。各 agent には REVIEW コンテキスト＋下記レンズ定義を渡し、
**施策候補 JSON の配列のみ**を返させる。

| # | レンズ | 調査内容 | 道具 |
|---|---|---|---|
| L1 | **現在の課題** | tech debt・TODO/FIXME・壊れかけ・命名/構造の不整合・テスト緩さ・未コミット債務 | Grep/Read/git |
| L2 | **セキュリティ** | 依存脆弱性(advisories)・secret 混入・unsafe/権限・hook の入力検証・コマンド注入・過剰権限 | cargo-deny/Grep/Read |
| L3 | **業界・他プロジェクト標準** | 同種 OSS（Rust CLI / Claude Code plugin / agent harness）が**実施している施策**で本 repo に**無い**もの | **WebSearch/WebFetch** + Read |
| L4 | **不足施策** | CI 網羅・test coverage・observability/telemetry・release/version 運用・docs parity・error handling | Read/Glob/git |
| L5 | **安全性・堅牢性** | failure mode・「never break a turn」不変条件の破れ・panic/unwrap・cross-platform・データ破損/競合・冪等性 | Grep/Read |

各 sub-agent が返す施策候補のスキーマ:
```json
[{
  "title": "短い命令形の施策名",
  "lens": "L1|L2|L3|L4|L5",
  "rationale": "なぜ必要か（1-2文）",
  "evidence": "逐語引用 / path:line / Web ソース URL（必須・無ければ出さない）",
  "severity": "high|medium|low",
  "effort": "xs|s|m|l|xl",
  "suggested_done": "完了とみなせる観察可能な条件"
}]
```

**sub-agent への厳命**: 実装提案ではなく**課題の発見**に徹する。証拠（引用/参照/URL）のない項目は出さない。
L3 は「他プロジェクトが現にやっている」根拠の URL を必ず添える。

### Phase 3 — 統合・重複排除・スコアリング（main / LLM）

全レンズの候補を集約し、**自分（main の LLM）で**:

1. **重複排除** — 同じ施策を別レンズが挙げたら 1 件に畳み、`lens` を併記。
2. **証拠フィルタ** — evidence の無い/弱い候補を落とす。
3. **spec-map 整合チェック** — 生き残った候補ごとに、evidence の `path:line` が指すファイルを
   `specguard map list --filter <touched-path> --json` で引く（`specguard` 未導入／非 0 終了／
   `.specguard/spec-map.toml` 未初期化なら **fail-soft でこのステップ全体を省略**し次へ進む —
   specguard は scout の必須依存ではない）。一致 entry に `spec_doc` があれば Read し、`test_files`
   があれば該当テストの assertion を確認する。候補が主張する「問題」が `spec_doc` に明記された
   意図的な挙動、または既存テストが green のままカバーしている挙動と一致するなら、**その候補を落とす**
   （確信が持てない場合は落とさず `note` に「spec/test 上は意図的の可能性、要確認」と付記して残す）。
   逆に spec/test と矛盾していれば、evidence に spec_doc の該当箇所も追記して残す。
   **spec-map に一致 entry が無い場合は候補を落とさず通常どおり残す**（未収録＝「無関係」ではなく
   「判定材料なし」であり、材料が無いことを却下理由にしない）。
4. **スコアリング** — 算術ランキングは自分（LLM）で手計算せず、決定論バイナリ `compass score` に委ねる。
   生き残った候補ごとに次を実行し、`goal_proximity` だけを見積もる（残りは自分で計算しない）:
   ```bash
   compass score --severity <candidate.severity> --effort <candidate.effort> \
     --lens <candidate.primary lens> --goal-proximity <0.0-1.0>
   ```
   - `goal_proximity` は **LLM の意味判断**（この施策が compass の gap をどれだけ埋めるか／north_star に
     どれだけ近づけるかを 0.0〜1.0 で見積もる）のみを担う。severity×proximity÷effort の算術は
     もう手計算しない — それは決定論バイナリの仕事。
   - 標準出力 `{"score": N}` の `score` で降順ソートし、ランキングを確定する。
   - **セキュリティ(L2)・安全性(L5) の重み上げはスコアラー側に内蔵済み**（手動の注意書きは不要になった）。
     L2/L5 レンズの候補は自動的に高スコアになるので、意図（壊さない・安全側を優先）は保たれる。
   - **fail-soft**: `compass` バイナリが無い／非 0 終了／`score` サブコマンド未対応（旧 compass）の場合は、
     従来どおり自分（LLM）で `(severity × goal への近さ) ÷ effort` を手計算し、
     **セキュリティ(L2)・安全性(L5) は重みを上げる**手動判断にフォールバックする（後方互換・never break a turn）。
5. **優先度付け** — `p0`(即対応) / `p1`(近いうち) / `p2`(いつか) のタグを付与。

### Phase 4 — 合意（AskUserQuestion / HOTL）

合意提示の前に、この選別ゲートを **`condukt policy answer` に通す**（condukt / flow と**同一**の shim。
まずグローバルな autonomy スイッチで縮退可否を確認し、autonomous なら per-gate verdict で判定する）:

```bash
condukt state autonomy-check   # autonomous なら exit 0 + {"autonomous":true}、そうでなければ exit 1 + {"autonomous":false}
```

- **exit 1（非 autonomous・既定）／ autonomy-check 未対応（exit 127）** → 従来どおり。スコア上位の施策
  （既定 8〜12 件）を `AskUserQuestion`（multiSelect）で提示し、**backlog に積むものを選ばせる**。各選択肢に
  `severity/effort/lens/priority` を要約表示する（後方互換。既定では必ず選別 Ask が出る）。
- **exit 0（autonomous）** → 選別ゲートを policy-answer に掛ける。施策採用は low-risk・可逆（backlog 項目は
  後で除去できる）なので既定 verdict は **auto**:
  ```bash
  OUT=$(condukt policy answer --risk low --reversible high --confidence high \
          --question "scout 施策のうち backlog に積むものは?" \
          --option "top-N を自動採用" --option "選別を人手に返す" --recommend 0 2>/dev/null)
  case $? in
    0) : ;;  # auto: 選別 Ask を省略し、スコア上位 N 件（既定 top 8、p0/p1 優先）を採用して Phase 5 へ auto-queue（自答は監査ログに残る）
    2) : ;;  # escalate: 上の exit 1 と同じ AskUserQuestion（multiSelect）で選ばせる
    3) : ;;  # block: 積まずに停止する
    *) : ;;  # 旧バイナリ（`answer` 無しの clap exit 2 も case 2 に落ちて安全）/ 不正入力 → 安全側 = AskUserQuestion
  esac
  ```
  採用した施策一覧は「autonomy: top-N を自動採用」として**サマリで明示**し、`condukt policy answers` で監査できる
  （黙って積まない）。安全側の不変:
  - `--dry-run` は autonomy でも**必ずここで停止**する（選別省略は「停止しない」ではない）。
  - `condukt` バイナリが無い / `answer` 未対応なら非 autonomous とみなし、従来どおり Ask を出す。

`--dry-run` ならここで停止し、提示だけで終了。

### Phase 5 — backlog へ書き出し

承認された施策を 1 件ずつ backlog に積む。同時に、**機械スコープ共有発見ストア（discovery store）に記録し、並行セッションの重複を回避**する（fail-soft）:

```bash
# 既に他セッションが発見した施策をスキップ（発見ストア確認）
DISCOVERED=$(compass discovery list --json 2>/dev/null || echo '[]')

# 各承認施策について
for 施策名 in "${APPROVED_TASKS[@]}"; do
  # 他セッションが既に所有している場合は skip（同じ title の行を探し、session_id が異なれば他セッション所有）
  if echo "$DISCOVERED" | jq -e --arg title "$施策名" '.[] | select(.title == $title and .session_id != env.SESSION_ID)' >/dev/null 2>&1; then
    echo "（スキップ: $施策名 は他セッションで既に発見）"
    continue
  fi
  
  # 本セッションの発見ストアに記録（失敗しても scout 続行 = fail-soft）
  compass discovery record --session-id "$SESSION_ID" --title "$施策名" 2>/dev/null || true
  
  # backlog に積む
  backlog add --title "$施策名" --project "$PWD" \
    --priority <p0|p1|p2> \
    --tag <lens> --tag scout \
    --notes "<rationale> / 証拠: <evidence> / 完了条件: <suggested_done>"
done
```

**発見ストア**（`~/.compass/<project_key>/discovery.jsonl`）は、同一機械上の複数 scout セッションが**同じ施策を重複 backlog 追加する**のを防ぐ side-channel（repo 書き込みではない）。ストア呼び出しが失敗しても scout は常に続行する（**never break a turn**）。

`--tag scout` を付け、scout 由来の施策と分かるようにする。`--notes` に証拠と完了条件を残し、
実行時（condukt の interpreter）が done_criteria を引けるようにする。クロスプロジェクトで
繰り返し検出される作業種別（worktree衛生・デプロイ後疎通確認・ネットワーク設定等）に該当する場合は
`docs/backlog-tag-taxonomy.md` の規約タグ（例: `worktree-hygiene`）も併せて付ける。

### Phase 6 — 実行引き渡し（autonomy で分岐）

backlog に積んだら、Phase 4 と**同じ autonomy スイッチ**で引き渡し方を決める（`condukt state autonomy-check`
の結果を Phase 4 で得ていればそれを再利用してよい）:

- **非 autonomous（既定）→ propose-then-confirm**。`/flow` を**提案**する（即実行はしない＝HOTL）:

  > 「scout が N 件の施策を backlog に積みました。`/flow` で source→executor を回して順に実装しますか？」

  ユーザーが承認したら `/flow`（または個別 `/condukt <施策>`）に引き渡す。scout はここで終了する。

- **autonomous → auto-handoff**。**積んだものがあれば**（Phase 5 で 1 件以上 backlog add した場合のみ）、
  提案の `AskUserQuestion` を省き、**そのまま `/flow` を起動**して source→executor ループへ直接引き渡す
  （scout→flow を人間 0 介入で連結する）。積んだ件数が **0 件なら起動しない**（空ループ防止）。
  安全側の不変: `--dry-run` は autonomy でも起動しない（Phase 4 で既に停止済み）。`flow`/`condukt`
  が無ければ起動できないので、その旨を報告して終了する。

いずれの場合も、実行ループと backlog ロックは **flow の責務**。scout 自身はループもロックも持たず、
`/flow` を起動したら**終了**する（併走しない）。autonomy の auto-handoff は「scout が flow を起動して
バトンを渡す」であって、scout が実行を続けるのではない。

---

## 早期脱出 / 失敗モード

| 状況 | 対応 |
|---|---|
| sub-agent が証拠なしの施策ばかり返す | 証拠フィルタで落とし、レンズを絞って 1 回だけ再調査 |
| 候補が 0 件（健全） | 「重大な不足施策は検出されず」と報告し、軽微な改善のみ提示 |
| `backlog` バイナリ不在 | 施策を Markdown で提示し、手動投入を案内（書き込みはスキップ） |
| WebSearch 不可（オフライン等） | L3 をスキップし「業界標準は未調査」と明示して残り4レンズで続行 |
| `--dry-run` | Phase 4 提示で停止、backlog に積まない |

## ハードルール（再掲）

- **scout は実装しない**。発見した施策の実行は backlog→/flow→condukt に委ねる。
- **証拠のない施策は出さない**（逐語引用 / `file:line` / Web URL のいずれか必須）。
- **/backlog や /flow と併走させない**（scout は書き込むだけでループを持たない）。autonomy の
  auto-handoff（Phase 6）は「`/flow` を**起動して終了**する」バトン渡しであり、併走ではない。
- **合意なく全件 backlog に積まない** — ただし *非 autonomous 既定時*。Phase 4 の AskUserQuestion を
  省略できるのは `condukt state autonomy-check` が autonomous を返したときのみで、その場合も採用した
  top-N をサマリで明示し、`--dry-run` では必ず停止する（静かな全件採用は禁止）。
