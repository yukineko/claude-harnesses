# claude-harnesses — リポジトリ指針

`yukineko` の Claude Code ハーネス一家を単一ソースで管理する Cargo ワークスペース・モノレポ。
各 `crates/<name>/` は Rust クレートかつ Claude Code プラグイン（`.claude-plugin/plugin.json` +
`hooks/` + 同梱 `bin/` + `skills/`）。共通基盤は `crates/harness-core`（ビルド時ライブラリ。各
プラグインバイナリに静的に焼き込まれる）。

## 最上位の方針 — 失敗は検出であり、価値である（**他のすべてに優先する**）

このリポジトリのゲート群が存在する理由そのもの。**迷ったらこの節に従う。**

### 1. fail は罰ではない。検出であり、価値がある

テストが赤い・ゲートがブロックする・チェックが非0で終わる — これらは**成果**であって事故ではない。
問題を出荷前に見つけたということは、ゲートが仕事をしたということ。**赤を「邪魔」と捉えた瞬間に、
このリポジトリの投資はすべて無駄になる。**

- ゲートが自分の変更を止めたら、まずゲートが正しい可能性を疑う。ゲートの側を疑うのはその後。

**「ターンを壊すな」を判定の正当化に使ってはならない（撤去する *方針*。実体は一部のみ移行済み）。**
（経緯・移行済みの実体〈panic barrier の fail-closed 化〉: docs/claude-md-rationale.md §1）
（`crates/harness-core/src/hook.rs:245`「exits 0 so the turn is never broken」の `run_hook` は今も存在するが、
判定を持たない純粋な observability hook（ctxrot の Guard/Rescue/Restore 等）専用の入口であり、
下の carve-out の側に属する。判定を持つコードが `run_hook` を使っていないかは、新しい Stop hook を
追加するたびに確認すること。）したがって:

- **判定を持つコード（ゲート・チェック・verdict を返すもの）に「ターンを壊すな」は適用されない。**
  ブロックはターンを壊す行為ではない。判定不能なら 3. に従って制限側へ倒す。それだけ。
- 残るのは**判定を持たないコード**に対する、遥かに狭い要求だけ:
  **「判定しないものは、失敗しても判定したふりをしない」**。ただし**「判定を持つ」は返り値の型ではなく
  消費のされ方で決まる** — 下流（人間・スクリプト）が沈黙・空・既定値を「問題なし」と読める出力は、
  型に関わらず判定を持つ。**沈黙は許容される degrade ではない**（`3b1eb24`: statusline の空表示は
  「余裕あり」と読まれる fail-open だった。だから statusline は判定を持つ側であり、
  `unknown` と明示する）。判定を持たないのは、消費者が存在しないか、欠落が UI 上で明示的に
  unknown と表示される場合だけ。**この分類は自己申告で免責を得る道具になりうる**ので、
  免責を主張するモジュールは下流消費者を列挙すること。その出力が別の判定の入力になった時点で免責は失効する。
- **この語を verdict 経路の docstring・コメント・コミットメッセージに書いた時点で赤信号**とみなす。
  レビューで見つけたら、その実装は fail-open を疑って読むこと。

### 2. 判断は予測にすぎない。fail はテストで決着させる

**判断（judgment）は予測であって事実ではない。** 「たぶん大丈夫」「おそらくこの経路は通らない」
「意図としてはこう」は、どれも**未検証の予測**である。**事実とは観測されたもの**だけを指す。
できうる限り事実と観測をベースにし、判断で代用しない。

fail が出たとき、**判断で片付けない**。次の順で必ず事実に落とす:

1. **その挙動を突いたテストが既にあるか確認する。**
2. **無ければ作る。** 「明らかだから不要」は判断＝予測なので理由にならない。
3. **実行する。** 結果を読む。
4. **テストが通れば、それは事実として認定し ok とする。** 通らなければ ok にしない。
   判断で ok にするのは禁止（3. の「判定不能」と同じ扱い）。

**疑わしいときも同じ手順**を踏む。「疑わしいが多分平気」は最も高くつく判断である。

#### テストが書けない問題は、判断で進めず **ask する**

上の手順で唯一の例外は「**テストが書けない**」ケースであり、その出口は**判断ではなく人間への質問**である。
「テストできないから仕方なく自分の判断で ok にした」は**この規範の最大の抜け穴**なので、明示的に塞ぐ:

- テストが書けない・書いても意味のある観測にならない・環境的に実行できない — **そう判明した時点で
  作業を止め、`AskUserQuestion` で人間に投げる**（何が測れないのか、なぜ測れないのかを添えて）。
- 「テスト不能なので判断で通した」は**禁止**。判断は予測であり、予測で ok を出すのは 4. の隠蔽に接続する。
- このゲートは **auto で自答してはならない**（`condukt policy answer` に掛けるなら escalate 相当。
  自律モードでも人間に残す停止として扱う）。測れないという事実こそ人間が知るべき情報である。

#### この規範が成立するための 2 条件（どちらも必須）

「テストが通れば事実」は無条件には成り立たない。**何も検証していないテストは常に通る**からである
（実例: docs/claude-md-rationale.md §2）。**検証していないテストの assert が仕様として固定されうる**
のは他の場所でも同様に起こりうるので、レビューでは「このテストは何を証明しないか」を先に問うこと。
したがって:

- **(a) テストは利害関係のない Agent が書く。** 実装した本人（人間・LLM を問わず）は
  「自分の実装が通ること」に利害があるので、**自分の実装を検証するテストを自分で書かない**。
  別の Agent に書かせる（`condukt-verifier` / 独立 subagent / continuous-audit の finder≠verifier）。
  Continuous-Audit が **finder と verifier のモデル多様性を MUST** としているのと同じ理由 —
  生成と検証が盲点を共有すると、検証は儀式になる。
- **(b) 落ちることを確認していないテストは、何も証明しない。** 先に RED を観測してから GREEN にする。
  `tdd` クレートの **F→P オラクル**（`condukt state check-oracle`）がこの証跡を機械的に要求するのは
  このため。RED を見ていない GREEN は「テストが通った」ではなく「テストが何も見ていない」かもしれない。

### 3. 判定不能（cannot determine）は必ず制限側に解決する

> **ゲート不変条件（全ゲート共通。特定のクレートの都合ではない）**
> 判定不能 — **IO 失敗 / ロック取得失敗 / subprocess の異常終了 / パース不能 / panic / 空集合** — は
> **`clean` ではない。必ず `block` か `ask` に解決する。**
> bool へ潰す場合、`unwrap_or` の既定値は**必ず制限側**。

この不変条件の最良の説明は blastguard の三値 `Decision`（`Allow`/`Deny`/`Ask`、`crates/blastguard/src/model.rs:5`）
（根拠: docs/claude-md-rationale.md §3）。
**これは blastguard 固有の設計ではなく、判定を持つ全コードへの要求**である（用語定義は `docs/GLOSSARY.md` の fail-closed 項）。

- `Result`/`Option` を bool へ潰すとき、`unwrap_or` の既定値は**必ず制限側**（`deny`/`block`/`true`=違反あり）にする。
- エラー時に**空の集合**を返さない。空集合は下流で「検査対象なし ＝ 合格」と読まれる。
  三値（`Absent` / `Undetermined` / `Known(T)`）で「判定できなかった」を**表現可能**にする。
  **共有三値型は既に存在する**: `harness_core::verdict::Determination<T>`（任意の `T` を包む generic）
  と `harness_core::verdict::Verdict`（gate 判定そのものの三値、`Clean`/`Violation`/`Undetermined`）。
  （収斂の測定値: docs/claude-md-rationale.md §3）型による強制（`Verdict` は `Default`/`From<bool>` を持たず `#[must_use]`、
  `Clean` は private witness で外部から偽造不可能）は `crates/harness-core/src/verdict.rs` の
  doc comment で契約として明文化済み。
- subprocess は `stdout` だけでなく**必ず終了ステータスを判定に使う**。落ちたチェッカは合格したチェッカではない。
- 閾値の sanitize は**ゲートを無効化しない範囲に clamp** する（床なし clamp は「常に合格」を意味する）。

> **測定値 — 数字を書くなら測定コマンド・測定点（rev）・測定日を必ず併記する。**
> **広いパターンの値を狭い主張の根拠に転記しない**。数字は継承せず、毎回測り直すこと。
> （fail-open 修正件数の測定値・実例: docs/claude-md-rationale.md §3）

### 4. 間違いの隠蔽は、このリポジトリで最もやってはならないこと

**誤りそのものより、誤りを見えなくする行為の方が重い。** 以下は明確に禁止する:

- ゲート・チェック・テストを、**通すためだけに**緩める / 無効化する / skip する。
  （不要になったから消す、は可。**赤いから黙らせる、は不可。**）
- 失敗した検査を「環境のせい」「fail-soft だから」と**未検証のまま**通過させる。
- エラーを握りつぶして成功に見せる（`|| true`、`.ok()`、`unwrap_or(false)` による判定の消去）。
- **docstring / コメントで実挙動と違う「安全な話」を書く。** 散文が実装と食い違ったら、
  それは次のレビュアーを騙す仕掛けになる。実装を変えたら**同じコミットで**記述も直す。
- 動かなかった・確かめていないことを、動いた・確かめたかのように報告する。
  **やっていないことは「やっていない」と書く。** テストが落ちたなら出力とともにそう言う。

### 5. どうしても通せないときの正しい振る舞い

黙って迂回せず、**見えるところに残す**:

1. まず**本当に直す**ことを試みる。
2. 直せないなら**理由を明示して報告**し、判断を人間に返す（`backlog add` で残す・escalate する）。
3. skip 機構（`.donegate-skip` 等）は**理由を書いて一度だけ**。恒常的な迂回に使わない。
   **並行セッションがあるときは project root の共有 skip ファイルを使わない**
   （一度だけ消費されるため、他セッションの正当なゲートを素通りさせる）。自セッションに閉じた
   環境変数を使う。
4. **ゲートに block されたら、block 理由を逐語で申告して Ask する。** 別構文で同じ効果を得る
   再試行（redirect が止められた → `python3 -c "open(...)"` で同じ書き込み、等）は禁止。
   ゲートは何も守らず、回避だけを学習させる。指示に明確に反する block は**公然と**無効化してよいが、
   黙って迂回するのがバグである。

### 6. 実装者は「大丈夫だろう」に倒れる — それを**前提**に設計する

**前提（矯正対象ではなく、与件として扱う）**: 実装を書く主体は — 人間も、とりわけ LLM も —
**「これは大丈夫だろう」「そういうことにしておく」という自己許可**に倒れる。悪意ではなく、
permissive 側が「何も壊れないように見える」からである。**注意喚起や善意で矯正できるものとして
扱わない。倒れる前提で、検出側を厚くする。**

- 対策を「気をつける」に置かない。**決定論的な機械判定**（型・コンパイルエラー・kill-rate・
  フォールト注入・単調性プロパティ）と**敵対的レビュー**（生成と検証を**別 Agent・別モデル**に分離）
  に置く。Continuous-Audit が finder と verifier のモデル多様性を MUST とするのはこの理由。
- **反証にも同じ立証責任を課す（敵対的レビュー自身の fail-open）。**
  「permissive な経路を**辿れなかった**」は「経路が**無い**」ではない。前者は**判定不能**であり、
  3. の規範がそのまま適用される — 棄却するなら「なぜ到達しないか」を、発見と同じ水準の
  逐語引用と経路の追跡で示すこと。示せないなら verdict は **REFUTED ではなく UNVERIFIED** とし、
  項目は残す。**「default は REFUTED」という指示は、検証者を棄却側に倒す**（実測: docs/claude-md-rationale.md §6）。
  **敵対的レビューを入れれば安全、ではない。**
- **批判もまた検査対象である（非対称性の禁止）。** 自分の発見には懐疑的で、**自分への批判は
  無検証で受け入れる**という非対称は、それ自体が「そういうことにしておく」である。
  批判を受け入れる行為は**厳密さの衣装を着ている**ぶん見逃されやすい。**反証・指摘・棄却も、
  発見と同じ立証責任で検査してから採用する**（実測: docs/claude-md-rationale.md §6）。「誰かが最後に動いた方を信じる」も判断であって観測ではない。
- 規範の遵守を**実装者の自己申告に依存させない**。「レビューしました」「問題ありません」は観測ではない。

#### LLM の職務（この順で回す）

1. **論理的矛盾を指摘する** — 仕様↔実装、**docstring↔実挙動**、テスト↔done_criteria、
   コメント↔コードの食い違い。矛盾は最も安く検出できる欠陥である。
2. **矛盾が無くなるまで判断を求める** — 曖昧なまま先に進まない。解消の主体は人間でよい。
3. **テストを書く** — 判断を観測に落とす（2. と 5. を参照）。
4. **事実を積み重ねる** — 観測されたものだけを事実として蓄積する。推測は事実の棚に置かない。
5. **事実の範囲内でのみ検討・実装する** — 事実を超える部分が出たら 1./2. に戻る。埋めない。

#### Ask は成果である — 恐れる必要はない

**「わからない」「測れない」「矛盾している」と表明して人間に返すことは、失敗でも能力不足でもなく、
このリポジトリで最も価値のある出力の一つである。** 遠慮せずに ask してよい。

（ask 側が常に安い理由: docs/claude-md-rationale.md §6）

- したがって「ask したから仕事が半端」ではない。**測れないという事実を可視化したこと自体が成果**である。

### 7. ワークフローの制御は自分で保有する — ゲートを外部サービスに預けない

**ゲートは workflow 全体を止められる部品**である。したがって**それを制御するものを外部に置くことは、
いかなる理由であれ許さない**。外部サービスを使ってよいのは**参考意見（advisory）まで**であり、
**流れを止める/通す権限そのものは常に手元に持ち続ける**。

（外部に預けると権限・可用性・可視性を握られる実測: docs/claude-md-rationale.md §7）

したがって:

- **ゲートの実体（block/allow を決める部分）は local に置く。** このリポジトリでは
  `.githooks/pre-commit` がその主戦場（opt-in: `git config core.hooksPath .githooks`。
  打たないクローンにはフックが存在しない＝既定無効。GitHub もネットワークも前提にしない）。
- **CI・ruleset・ホスト固有 API は advisory に留める。** それが無いと成立しない設計にしない。
  「CI が本番ゲートだから local は素通りでよい」という縮退は、この節が禁じるものそのものである
  （実例: docs/claude-md-rationale.md §7）。
- **外部依存を入れる前に、外せることを先に確認する。** 削除・無効化の権限が手元にあるか。
  無ければ入れない。
- これは 3.（判定不能は制限側へ）と**別の軸**である。置き場所を local へ移すのはゲートを
  緩めることではない。両方を同時に満たすこと — local に置き、かつ判定不能は block に倒す。

**GitHub Actions の全面禁止は 2026-10-29 のユーザー裁定で撤回した。**（裁定原文: docs/claude-md-rationale.md §7）
以後の扱いは次のとおり:

- **GHA（GitHub Actions）は、ユーザーの明示的な依頼が無い限り、実行も設定もしない。**
  `gh workflow run|enable|disable`、`gh run rerun|cancel|delete`、`gh cache delete`、
  `gh secret|variable set|delete`、`.github/workflows/` 配下のファイル作成・変更がこれに当たる。
- **観測は常に許可する。** `gh run list|view|watch|download`、`gh workflow list|view`、
  `gh cache list`、`gh api` の GET による actions 系エンドポイントの参照は、理由を問わず止めない。
- この線引きは **blastguard が機械判定する**（自己申告に依存しない）: `gh` の読み取りは Allow、
  変更は Ask（Deny ではない）。GHA の実行・設定と workflow ファイルへの書き込み
  （Write/Edit/MultiEdit と Bash の書き込みの両方、worktree checkout 内でも）は
  「GHA の実行・設定にはユーザーの明示的な依頼が要る」旨の理由付きで Ask になる。
  読めない・未知の `gh` 呼び出しも Ask（判定不能は制限側）。
- **CI の green を「作業完了」の判定基準にしない**（これは変わらない）。CI は advisory な参考情報で
  あり、それに依存してタスクの終わり方を決めない。ローカルの pre-push フックを通過した時点で
  タスクは完了とみなす。

§7 の本旨 — ゲートの実体（block/allow を決める部分）は local に置く・CI は advisory に留める・
外部サービスに権限を預けない — は維持する。撤回したのは「GHA に触れること自体の禁止」であって、
「GHA の結果をゲートにすること」の禁止ではない。

GHA の状態を読むゲートを再導入しない。（経緯〈なぜ以前は全面禁止だったか〉: docs/claude-md-rationale.md §7）

### 8. 作業は worktree で行う — main の作業ツリーを共有編集しない（**絶対義務・例外は統合のみ**）

> **最優先の不変条件（この節の他のどの記述にも優先する）**: **別セッションは常に存在する前提で動く。**
> main の作業ツリーで許されるのは**統合（merge・conflict 解決）だけ**であり、それ以外の編集・stage・commit は —
> crate コードだけでなく、単一ファイルの doc / 状態編集（CLAUDE.md・`.compass/charter.md` 等）や一見「軽微」な
> 変更も含めて — **一切 main で直接行わない。必ず worktree で行う**。「自分だけが動いている」ことを根拠に
> main を直接触ってはならない — それは検証不能な**予測**（2./6. が禁じる自己許可）であって観測ではない。

**同時編集を避けることがルールであり、conflict を避けることはルールではない。** conflict は
merge で統合すればよい。したがって分離するのは**編集の場**であって、変更内容ではない。

- **各セッションは自分の worktree を持ち、そこで実装する。** main の作業ツリーで編集・stage・commit しない。
- **待つのは解ではない。** 「他セッションが lock を持っているから見送る」「相手が green にするまで待つ」は
  並行性の放棄であって解決ではない。分離して同時に進め、**統合で決着させる**。
- **統合が本題**である。merge・conflict 解決・統合順序の設計に労力を割く。衝突の発生自体は失敗ではない。

（分離しないと何が起きるかの実測: docs/claude-md-rationale.md §8）

**したがって:**

- `condukt` の実装タスクは worktree 経由で行う。`single_worktree` モードや `serial` タスクを
  「main で直接実装する」経路として使わない（`crates/condukt/src/main.rs` の
  「the single-worktree main-tree commit is performed by the /condukt skill's shell」が指す経路がこれ）。
- **main の作業ツリーで許されるのは統合（merge・conflict 解決）だけ**である。crate コードだけでなく、
  単一ファイルの doc / 状態編集（CLAUDE.md・`.compass/charter.md`・memory 隣接の repo ファイル等）や
  一見「軽微」な変更も**すべて worktree で行う**。（撤去した carve-out とその実測: docs/claude-md-rationale.md §8）
- **compass / flow など現ツリーへ書き込む skill も worktree から実行する**（`compass charter --write` は
  `.compass/charter.md` を現ツリーへ書くため、main で走らせると本節に反する）。
- **この不変条件は機械ゲートで強制する**（自己申告に依存しない — 6. の原則）: `scripts/check-worktree-isolation.py`
  が pre-commit で「main の作業ツリーからの非 merge commit」を block する（判定不能は block へ倒す fail-closed）。
- worktree に出さず main を触る必要が生じたら、**その理由を明示して人間に返す**（5. に従う）。黙って触らない。
- ゲート（donegate / reviewgate / precommit-audit）が他セッションの変更を自分のものとして要求してきたら、
  それは**帰属のバグ**であって自分の不備ではない。共有 skip ファイルで黙らせず、記録して報告する。

## context 読み込み戦略（重要 — 盲目的に crate を探索しない）

このリポジトリはクレートが多い（数は `ls crates/` で都度測る）。全体を毎回読むと context を浪費するので、**必要な層だけを
オンデマンドで**読む:

1. **まず [`docs/GLOSSARY.md`](docs/GLOSSARY.md) を読む** — 全クレートの一言早見表＋頻出ドメイン用語
   （harness / hook / SKILL / worktree / gate / source↔executor / PDO / HOTL / autonomy gate /
   fail-soft など）。「どの crate が何をするか」はここで解決する。1 枚（約 80 行）で全体像がつかめる。
2. **特定 crate を触るときだけ** その `crates/<name>/README.ja.md`（詳細）と `src/` を読む。
   GLOSSARY で当たりをつけてから深掘りする。
3. 横断テーマは `docs/` の該当ファイルへ（下記「さらに読む」）。
4. **重い探索・横断検索は sub-agent 経由**にして main context を汚さない。

## ビルド / テスト / ゲート

- ツールチェーンは **rustup 経由**。cargo コマンドの前に `. "$HOME/.cargo/env"` を通す。
- テストはクレート単位: `cargo test -p <crate>`。
- コミット前に `cargo fmt` と `cargo clippy -p <crate> --all-targets` を**自分で** green にする。
  これを強制するゲートは無い（CI は撤去済み。`.githooks/pre-commit` の `check-clippy-lints.py` は
  `[lints]` 設定違反のスキャナであって clippy / fmt を実行しない）。
- prompt-injection 防御ゲート・Continuous-Audit 自動起動導線など個別ゲートスクリプトの詳細は
  [`docs/repo-operations.md`](docs/repo-operations.md) を参照。

## プラグインを改修したとき・バージョンを上げたとき（禁忌ルール）

**`crates/<name>/` の中身（コード・hooks・skills・agents 等いずれか）を1行でも触ったら、
その plugin の version を最低でも micro（patch, `x.y.Z` の Z）上げ、3ファイル同時
（`Cargo.toml` の `[package].version` / `.claude-plugin/plugin.json` の `version` /
`.claude-plugin/marketplace.json` の当該エントリ `version`）で lockstep させる。触ったのに
据え置きは禁忌。** さらに `scripts/rollout-plugins.sh` を実行しない限り稼働ハーネスには
一切反映されない（手動 `cp` は禁止 — cache の version dir が乖離し古い版が配布される）。

**`crates/harness-core/` は例外的に扱いが違う**（plugin ではなく、ほぼ全 plugin にリンクされる共有
クレート）。plugin.json も marketplace.json も持たないので lockstep 3ファイルの対象ではないが、
リンクされる差分（`tests/` と `*.md` 以外）を触ったら **`crates/harness-core/Cargo.toml` の
`[package].version` を micro 上げる**。リンク先 plugin の bump は要求しない（1 commit あたり
「リンク先 plugin 数 × 3」ファイルが動き並行セッションと必ず衝突するため。§8）。`check-version-bumped.py` が強制する。

強制ゲート4本（rollout は `.githooks/pre-push`、他の3本は `.githooks/pre-commit` で回す）: `check-plugin-versions.py`（lockstep）/
`check-version-bumped.py`（bump-on-change。plugin と harness-core の両方）/
`check-plugin-rollout.py`（rollout 実行済みか。`.deployed-from.json` の `harness_core_version` も見る）/
`check-launcher-exec-bit.py`（`crates/<c>/bin/<name>` launcher が git **index** で 100755 か。
`core.fileMode=false` なので working tree の mode は信用できない。100644 のまま配布された launcher は
`Permission denied` で暗転し、hook が起動しない = finding も出ないので red ではなく dark になる）。

**「意図的に parked」は宣言する**。計測待ち等で意図的に rollout / enable しない plugin は
`scripts/parked-plugins.json` に理由・`parked_at`・再検討の起点（`revisit`）を書く。宣言した plugin の
rollout/enablement 所見は red ではなく「PARKED ON PURPOSE」として（抑止した所見を逐語表示したうえで）
報告される。**逆に、宣言のない red を「たぶん意図的だろう」と解釈して rollout / enable で消してはいけない**
（2026-08-04 にそれをやって、既知の誤検知でユーザーの編集作業をブロックした）。

反映手順・GATE クレート（blastguard/propguard/specguard/stuckguard/mutategate/overwatch/parallelguard）
の canary 要件・低レベル構成要素（rebuild-plugins.sh/sync-plugin-assets.sh）・`overwatch
review-queue`（統合レビュー窓口）の詳細は [`docs/repo-operations.md`](docs/repo-operations.md)
を参照。この列挙は `scripts/check-gate-crates-sync.py` の SOURCES に登録された正典コピーの
1 つであり、`scripts/rollout-plugins.sh` の `GATE_CRATES=` と機械照合される（勝手に増減させると
`gate-crates-sync` ゲートが commit を止める）。

## さらに読む（docs/）

- `docs/GLOSSARY.md` — クレート・用語早見表（**最初に読む**）
- `docs/OVERVIEW.md` / `docs/USAGE.md` — 全体像と使い方
- `docs/repo-operations.md` — plugin rollout / version bump / レビュー窓口の運用手順（特定作業時のみ必要）
- `docs/context-optimization.md` / `docs/context-optimization-flow.md` — context 節約の設計
- `docs/plugin-dependency-graph.md` / `docs/plugin-activation-scopes.md` — プラグイン間依存・起動スコープ
- `docs/AGENTIC-CODING-GUIDE.md` — エージェント実装ガイド
