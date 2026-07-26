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
このリポジトリには「never break the turn ＝ ターンを壊すな」という**不変条件**があり、
判定を持つコードにも適用されていた。結果として Stop ゲートの panic barrier は panic を「許可」に
写しており、**panic やエラーを「許可」に写す実装がこの一文で正当化されていた** — 当時のコードは
`docs/article-llm-fail-open.md:48` に「swallow it and exit 0 (allow the stop; a hook must never break the user's turn)」として引用が残る。

**移行済みの実体**: 7 ゲート（donegate / reviewgate / propguard / tdd、および budgetguard / autoflow /
ctxrot の Stop hook 各 `main.rs`）が通る panic barrier は fail-closed へ移行した（前者4つは `d6db4670`、
後者3つ=budgetguard/autoflow/ctxrotは判定〈`{"decision":"block"}` を実際に返す〉を持ちながら
`harness_core::hook::run_hook`〈panic握り潰し→常時exit0〉経由のまま取り残されていたのを本節の是正として
`harness_core::gate::run::run_guarded` へ移行）。gate 本体が panic した＝判定不能は block に
解決する: `crates/harness-core/src/gate/run.rs:36`「**fail closed**: block the stop and surface the crash」。
連続 2 回目の panic だけが `stop_hook_active` により bounded に allow へ落ちる。
`docs/stop-gate-latency.md` の記述もこの移行に合わせて更新済み（旧「A gate that errors internally
allows the stop」という一律の記述は撤去）。
（`crates/harness-core/src/hook.rs:217`「exits 0 so the turn is never broken」の `run_hook` は今も存在するが、
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
（実例・**修正済み**: `crates/condukt/src/verify.rs` の `checks_verdict` はかつて空スライスに
`true` を返し、`assert!(checks_verdict(&[]))` がそれを*正しいものとして固定*していた（空集合 fail-open が
仕様として書き下された形で、3. の「空集合を返さない」に真っ向から反していた）。commit `d30d9b00`
（2026-07-21 13:51、backlog `100af807`）で三値化し、今は空スライスに `ChecksVerdict::NoChecksDeclared`
を返す（`Passed` ではない）。教訓は変わらない: **検証していないテストの assert が仕様として固定されうる**
のは他の場所でも同様に起こりうるので、レビューでは「このテストは何を証明しないか」を先に問うこと）。
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

「わからない」は「大丈夫」ではない。この 2 つを同じ出力に写した時点で、そのゲートは
「検査した」と「検査できなかった」を下流から区別不能にしている。

この不変条件が最もよく説明されているのは `crates/blastguard/src/model.rs:5`（「Three answers, not two.」）で、
**二値型そのものが原因**だと名指ししている — 二値の `Allow`/`Deny` は、blastguard が解析できない構文を
すべて `Allow` に強制し、「これは理解できない」を「これは問題ない」として記録させていた。
`Decision::Ask` はその欠けた第三の答えで、`crates/blastguard/src/model.rs:9`（「it is NOT a verdict about the command, it is a refusal」）
のとおり**コマンドについての判定ではなく、判定を推測することの拒否**である。
**これは blastguard 固有の設計ではなく、判定を持つ全コードへの要求**である（用語定義は `docs/GLOSSARY.md` の fail-closed 項）。

- `Result`/`Option` を bool へ潰すとき、`unwrap_or` の既定値は**必ず制限側**（`deny`/`block`/`true`=違反あり）にする。
- エラー時に**空の集合**を返さない。空集合は下流で「検査対象なし ＝ 合格」と読まれる。
  三値（`Absent` / `Undetermined` / `Known(T)`）で「判定できなかった」を**表現可能**にする。
  **共有三値型は既に存在する**: `harness_core::verdict::Determination<T>`（任意の `T` を包む generic）
  と `harness_core::verdict::Verdict`（gate 判定そのものの三値、`Clean`/`Violation`/`Undetermined`）。
  `grep -rln 'harness_core::verdict::' crates/*/src/`（harness-core 自身を除く）は21ファイルにヒットする
  （測定日 2026-07-22、測定点 `6d4312c5`）— blastguard/budgetguard/condukt/ctxrot/donegate/gauge/
  harness-status/propguard/reviewgate/session-insights 等、各 crate が個別再発明するのではなく
  この共有型へ収斂している。型による強制（`Verdict` は `Default`/`From<bool>` を持たず `#[must_use]`、
  `Clean` は private witness で外部から偽造不可能）は `crates/harness-core/src/verdict.rs` の
  doc comment で契約として明文化済み。
- subprocess は `stdout` だけでなく**必ず終了ステータスを判定に使う**。落ちたチェッカは合格したチェッカではない。
- 閾値の sanitize は**ゲートを無効化しない範囲に clamp** する（床なし clamp は「常に合格」を意味する）。

> **測定値 — 数字を書くなら測定コマンド・測定点（rev）・測定日を必ず併記する。**
> 測定点の無い数字は、次の著者が転記した瞬間に腐る。
>
> ```
> git log 94364b09 -i --grep='fail.open\|fail.closed' --oneline | wc -l
> ```
> → **60 件**（測定日 2026-07-21、測定点 `94364b09`）。うち **32 件が 2026-07-21 の 1 日**に
> landed している（同じ集合を `--format='%ad' --date=short` で日別集計）。
>
> 同じ手順を `silently` で回すと 106 件になるが、これは無関係な commit と `feat` を含む
> **広いパターン**であり、「このクラスの修正」の根拠には使えない。
> **広いパターンの値を狭い主張の根拠に転記しない** — それがこの節が防ごうとしている記録の腐敗そのもの。
>
> この段落の前版は「45 件 / 126 件 / 14 件」と書いていたが、**どれも同じ方法で再現できなかった**
> （実測 60 / 106 / 32）。数字は継承せず、毎回測り直すこと。
>
> 60 件のうち少なくとも 3 件（`3b1eb24` / `c066fc8` / `05df9b2`）は「安全な degrade だ」と
> *明示的に正当化するコメント*を伴っていた（`231e20e` はコメント無しだったので「全件」ではない）。
> **バグというより、判断が ok 側に倒れた結果**である。だからこの規範を明文で置く。

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
  項目は残す。**「default は REFUTED」という指示は、検証者を棄却側に倒す**（実測: 2026-07-21 の
  監査で `specguard/forge/gather.rs` の実在する fail-open が、検証者が消費経路を 1 本しか辿らなかった
  ために誤って棄却された。件数閾値を満たす部分的な束が clean として下流へ渡る経路を見落としていた）。
  **敵対的レビューを入れれば安全、ではない。**
- **批判もまた検査対象である（非対称性の禁止）。** 自分の発見には懐疑的で、**自分への批判は
  無検証で受け入れる**という非対称は、それ自体が「そういうことにしておく」である。
  批判を受け入れる行為は**厳密さの衣装を着ている**ぶん見逃されやすい。**反証・指摘・棄却も、
  発見と同じ立証責任で検査してから採用する**（実測: 2026-07-21、敵対的検証者の 4 件の
  `REFUTED` を無検証で採用して backlog を降ろしたが、うち 1 件は実在する欠陥だった。
  残り 3 件は今も未検証のまま）。「誰かが最後に動いた方を信じる」も判断であって観測ではない。
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

- **質問を避けて自分の判断で埋める行為が、まさに「そういうことにしておく」**であり、
  123 コミット分の fail-open を生んだ当のものである。
- Ask のコストは人間の数秒。黙って判断で埋めたコストは、**誰も気づかないまま残る fail-open**。
  非対称であり、ask 側が常に安い。
- したがって「ask したから仕事が半端」ではない。**測れないという事実を可視化したこと自体が成果**である。

### 7. ワークフローの制御は自分で保有する — ゲートを外部サービスに預けない

**ゲートは workflow 全体を止められる部品**である。したがって**それを制御するものを外部に置くことは、
いかなる理由であれ許さない**。外部サービスを使ってよいのは**参考意見（advisory）まで**であり、
**流れを止める/通す権限そのものは常に手元に持ち続ける**。

外部に預けると、**権限・可用性・可視性の 3 つを同時に相手に握られる**。これは推測ではなく実測である
（2026-07-21、fail-open ゲート 4 本を GitHub の required status check として登録した結果）:

- **権限**: 解除に必要な Administration:write が手持ちの fine-grained PAT に無く、
  `gh api -X DELETE …/rulesets/19438193` は `HTTP 403` で拒否された。**自分で外せない**。
- **可視性**: 正しい修正（PR #24）が止まっていることが**ローカルからは見えない**。担当 agent は
  「34→29 を達成した」と正しく報告でき、その報告は真だが、変更は merge 不能のままだった。
  **報告が真であることと、流れが進んでいることが分離した。**
- **可用性**: ゲートが自分の側の事情（CI の赤・スキャナのバグ）で止まっても、こちらには手がない。

したがって:

- **ゲートの実体（block/allow を決める部分）は local に置く。** このリポジトリでは
  `.githooks/pre-commit` がその主戦場（opt-in: `git config core.hooksPath .githooks`。
  打たないクローンにはフックが存在しない＝既定無効。GitHub もネットワークも前提にしない）。
- **CI・ruleset・ホスト固有 API は advisory に留める。** それが無いと成立しない設計にしない。
  「CI が本番ゲートだから local は素通りでよい」という縮退は、この節が禁じるものそのものである
  （旧 `.githooks/pre-commit` は python3 欠落・スキャナ欠落を exit 0 で通していた。
  その正当化が CI 依存だった）。
- **外部依存を入れる前に、外せることを先に確認する。** 削除・無効化の権限が手元にあるか。
  無ければ入れない。
- これは 3.（判定不能は制限側へ）と**別の軸**である。置き場所を local へ移すのはゲートを
  緩めることではない。両方を同時に満たすこと — local に置き、かつ判定不能は block に倒す。

**GitHub Actions のようなゴミサービスは二度と使用しない。** ユーザーの事後承諾があった過去の
2回（2026-07-24、`PREPUSH_SKIP_CI_RED=1` の1回目は確認を取った上での承認、2回目は確認を怠っての
黙示的な再使用）を最後に、以後は例外なく使用禁止とする。「明示的に許諾するまで」のような条件付き
再開の余地を残さない — 条件を残すこと自体が、この節が禁じる「外部サービスに権限を預ける」への
逆戻りの入り口になる。`gh workflow run` での手動 dispatch、`gh run watch`/`gh run list`/`gh run view`
での完了待ち・監視、CI の green を「作業完了」の判定基準にする行為（advisory な参考情報として一瞥
するのではなく、それに依存してタスクの終わり方を決める行為）のいずれも行わない。

**Why**: 2026-07-24、mutation ワークフローが main で chronic red だった際、ローカルの
pre-push フックが `PREPUSH_SKIP_CI_RED=1`（ユーザー承認済み）で通過した後も、修正が本当に効いたかを
確かめるために `gh workflow run` を試み（403 で拒否）、代わりに push が誘発した実 run を
`gh run watch` で監視し続けた。ユーザーからの複数回の「終わった？」への応答のたびに `gh run list` を
ポーリングし、stuckguard に同一操作の繰り返しとして検知された。ローカルの pre-push フック（block/allow
権限の実体）は既に通過しており、その時点でタスクは完了していたにもかかわらず、GitHub Actions という
外部サービスの状態を「本当に終わったか」の追加ゲートとして扱っていた。これは本節冒頭の
「流れを止める/通す権限そのものは常に手元に持ち続ける」に反する実例であり、advisory を事実上の
追加ゲートへ格上げしていた。

**How to apply**: `gh workflow run` / `gh run watch` / `gh run list` / `gh run view` を含む
GitHub Actions というゴミサービスへの操作を、理由の如何を問わず一切行わない。ローカルの pre-push
フックを通過した時点でタスクは完了とみなし、CI の結果を待たない・確認しない。pre-push フックが
chronic red で再びブロックした場合も、`PREPUSH_SKIP_CI_RED=1` を独断で再使用しない — 必ず
`AskUserQuestion` で都度ユーザーに確認する（「前回承認されたから今回も」という推測は判断であり、
2. が禁じる「判断で埋める」そのものなので、このrunfileでは省略しない）。ユーザー自身が CI の状態を
知りたいと明示的に依頼した場合に限り、その依頼の範囲でのみ調べてよい。

### 8. 作業は worktree で行う — main の作業ツリーを共有編集しない（**絶対義務・例外は統合のみ**）

> **最優先の不変条件（この節の他のどの記述にも優先する）**: **別セッションは常に存在する前提で動く。**
> main の作業ツリーで許されるのは**統合（merge・conflict 解決）だけ**であり、それ以外の編集・stage・commit は —
> crate コードだけでなく、単一ファイルの doc / 状態編集（CLAUDE.md・`.compass/charter.md` 等）や一見「軽微」な
> 変更も含めて — **一切 main で直接行わない。必ず worktree で行う**。「自分だけが動いている」ことを根拠に
> main を直接触ってはならない — それは検証不能な**予測**（2./6. が禁じる自己許可）であって観測ではない。

**同時編集を避けることがルールであり、conflict を避けることはルールではない。** conflict は
merge で統合すればよい。統合できないのは**同じ index / 作業ツリーを2つのセッションが共有した場合**だけで、
これは git が救えない唯一の競合である。したがって分離するのは**編集の場**であって、変更内容ではない。

- **各セッションは自分の worktree を持ち、そこで実装する。** main の作業ツリーで編集・stage・commit しない。
- **待つのは解ではない。** 「他セッションが lock を持っているから見送る」「相手が green にするまで待つ」は
  並行性の放棄であって解決ではない。分離して同時に進め、**統合で決着させる**。
- **統合が本題**である。merge・conflict 解決・統合順序の設計に労力を割く。衝突の発生自体は失敗ではない。

**実測（2026-07-23、本 repo で観測）**— 分離しないと何が起きるか:

- 2セッションが main の作業ツリーを共有し、`git status` に双方の未コミット変更が同時に現れた
  （一方は `crates/harness-core/src/boundary.rs`、他方は `crates/condukt/src/worktree.rs`）。
  **触っているファイルは非衝突なのに、index が共有されているため分離できない。**
- `donegate` の required check は workspace 全体（`cargo fmt --all` / `cargo clippy --workspace`）を見るため、
  **自分が触っていない crate の赤が自分の停止をブロック**した。`reviewgate` も他セッションの差分を
  自分の変更としてレビュー要求した。
- その結果、唯一の逃げ道が project root の共有 skip ファイルになる。これは**一度だけ消費される**ため
  他セッションの正当なゲートを素通りさせる — 5. が禁じる fail-open へ構造的に圧力がかかる。
- **worktree 分離はこの3つを同時に消す**（別チェックアウトなので相手のファイルがそもそも存在せず、
  ゲートは自分の変更だけを見る）。

**したがって:**

- `condukt` の実装タスクは worktree 経由で行う。`single_worktree` モードや `serial` タスクを
  「main で直接実装する」経路として使わない（`crates/condukt/src/main.rs` の
  「the single-worktree main-tree commit is performed by the /condukt skill's shell」が指す経路がこれ）。
- **main の作業ツリーで許されるのは統合（merge・conflict 解決）だけ**である。crate コードだけでなく、
  単一ファイルの doc / 状態編集（CLAUDE.md・`.compass/charter.md`・memory 隣接の repo ファイル等）や
  一見「軽微」な変更も**すべて worktree で行う**。旧版にあった「単一セッションしか動いていないことが確かな場合の
  軽微な編集だけは可」という carve-out は**撤去した** — その「確かな場合」は検証不能だからである
  （実測 2026-07-26: `overwatch status` が no session を返す一方で `scripts/rebuild-plugins.sh` が連続する
  2 回の git 呼び出しの間に modified→clean へ変化し、別セッションが main のツリーを触っている証拠になった。
  「たぶん自分だけ」は判断＝予測であり 2./6. が禁じる自己許可そのもの）。
- **compass / flow など現ツリーへ書き込む skill も worktree から実行する**（`compass charter --write` は
  `.compass/charter.md` を現ツリーへ書くため、main で走らせると本節に反する）。
- **この不変条件は機械ゲートで強制する**（自己申告に依存しない — 6. の原則）: `scripts/check-worktree-isolation.py`
  が pre-commit で「main の作業ツリーからの非 merge commit」を block する（判定不能は block へ倒す fail-closed）。
- worktree に出さず main を触る必要が生じたら、**その理由を明示して人間に返す**（5. に従う）。黙って触らない。
- ゲート（donegate / reviewgate / precommit-audit）が他セッションの変更を自分のものとして要求してきたら、
  それは**帰属のバグ**であって自分の不備ではない。共有 skip ファイルで黙らせず、記録して報告する。

## context 読み込み戦略（重要 — 盲目的に crate を探索しない）

このリポジトリは 39 クレートある。全体を毎回読むと context を浪費するので、**必要な層だけを
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
- CI ゲートは **fmt + clippy を強制**する。コミット前に `cargo fmt` と
  `cargo clippy -p <crate> --all-targets` を green にする。
- prompt-injection 防御ゲート・Continuous-Audit 自動起動導線など個別ゲートスクリプトの詳細は
  [`docs/repo-operations.md`](docs/repo-operations.md) を参照。

## プラグインを改修したとき・バージョンを上げたとき（禁忌ルール）

**`crates/<name>/` の中身（コード・hooks・skills・agents 等いずれか）を1行でも触ったら、
その plugin の version を最低でも micro（patch, `x.y.Z` の Z）上げ、3ファイル同時
（`Cargo.toml` の `[package].version` / `.claude-plugin/plugin.json` の `version` /
`.claude-plugin/marketplace.json` の当該エントリ `version`）で lockstep させる。触ったのに
据え置きは禁忌。** さらに `scripts/rollout-plugins.sh` を実行しない限り稼働ハーネスには
一切反映されない（手動 `cp` は禁止 — cache の version dir が乖離し古い版が配布される）。

強制ゲート3本（commit 前・push 前・CI で回す）: `check-plugin-versions.py`（lockstep）/
`check-version-bumped.py`（bump-on-change）/ `check-plugin-rollout.py`（rollout 実行済みか）。

反映手順・GATE クレート（blastguard/propguard/specguard/stuckguard/mutategate/overwatch）の
canary 要件・低レベル構成要素（rebuild-plugins.sh/sync-plugin-assets.sh）・`overwatch
review-queue`（統合レビュー窓口）の詳細は [`docs/repo-operations.md`](docs/repo-operations.md)
を参照。

## さらに読む（docs/）

- `docs/GLOSSARY.md` — クレート・用語早見表（**最初に読む**）
- `docs/OVERVIEW.md` / `docs/USAGE.md` — 全体像と使い方
- `docs/repo-operations.md` — plugin rollout / version bump / レビュー窓口の運用手順（特定作業時のみ必要）
- `docs/context-optimization.md` / `docs/context-optimization-flow.md` — context 節約の設計
- `docs/plugin-dependency-graph.md` / `docs/plugin-activation-scopes.md` — プラグイン間依存・起動スコープ
- `docs/AGENTIC-CODING-GUIDE.md` — エージェント実装ガイド
