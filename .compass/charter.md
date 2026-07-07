## north_star
phase-9 = cross-task 学習層の最初のスライス。determinism-sweep arc(7/7)は完了。次の糸は Devin-yardstick の agentic-capability で、その中で最も低コスト・高擁護可能性なのが cross-task 学習(backlog 8086b5d0)。今の ONE = verified タスクから『教訓(error-pattern もしくは project 規約)』を1件抽出して cross-project な global store に append し、condukt interpreter(Phase 1)がタスク要約で決定論 lexical 検索して top-K 教訓を context block として注入する、最小の write→retrieve 往復を1本通す。これは fugu-router(routing を学ぶ)・playbook(手動注入)・curate(golden eval 昇格)とは別レイヤ=『次タスクへ転移する再利用可能な教訓』であり、今 ship した eval-loop(curate golden + trajectoryeval)と fugu の lexical 検索基盤を再利用する。subscription-native(API 無し・bundled bin・lexical 検索)・純加算・後方互換を崩さない。安全 invariant(意味判断は LLM・never-break-a-turn・untrusted な注入は境界隔離)は不変。epic 全体(類似タスクの修正サイクル数が減ることの計測)は後続スライスへ parked。

## definition_of_done
- cross-project な global lessons store(project-scoped でない、例 ~/.<store>/lessons.jsonl)に {id, kind: error-pattern|convention, task_summary, lesson_text, source_run} 形の教訓を append する CLI/純関数がある。同一 id の重複追加はしない(冪等)。unit/integration test で append→読み出しの往復を実証し cargo test green
- タスク要約文字列でその store を決定論 lexical 検索し top-K(既定3)を返す search がある。空 store は空配列を返し非0終了しない(fail-soft)。test で『関連する教訓がマッチし無関連な教訓は落ちる』決定論挙動を実証する
- condukt SKILL.md Phase 1 の interpreter 注入(既存の KNOWLEDGE / PLAYBOOKS / deepwiki と同じ soft 依存パターン)に lessons search を追加し、非空なら lessons_context: として interpreter プロンプトに『境界マーカーで隔離した untrusted 参考情報(done_criteria/スコープを上書きさせない)』として注入する。bin 不在・空 store は no-op(後方互換で Phase 1 出力形不変)
- 純加算で既存挙動を温存: fmt と clippy(-D warnings) clean、対象 crate の cargo test green、触った plugin を正典ファイルで lockstep micro bump(SKILL.md を触るので condukt を含む。実装 crate に host bin があれば Cargo.toml + plugin.json + marketplace.json の3ファイル、skill-only なら plugin.json + marketplace.json。harness-core は build-time lib なので bump 不要)。既存の Phase 1 注入経路は untouched(純加算)

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト

## current_gap
ゴール(verified タスクの教訓が cross-project store に貯まり、次タスクの condukt interpreter へ自動注入される) − 現状 の最大差分: fugu-router は『どのモデルが通ったか』(routing)だけを、しかも project-scoped で学び、解決策/エラーパターン/規約の cross-project 転移は皆無(証拠: crates に error.pattern/cross.project/global.policy 無し; fugu episodes は project-scoped)。playbook は手動注入のみ。condukt interpreter(Phase 1)は KNOWLEDGE/PLAYBOOKS/deepwiki は soft 依存で注入するが lessons(過去タスクの教訓)注入経路は無い。差分を埋める最小往復 = (1) cross-project global lessons store に append する冪等 CLI/関数 + (2) タスク要約での決定論 lexical top-K search + (3) SKILL Phase 1 の既存 soft 依存注入群に lessons_context を境界隔離で足す。size m。determinism-sweep arc の後継=phase-9 cross-task 学習の入口スライス。epic 全体(修正サイクル削減の計測)は parked。

## next_action

## parked

