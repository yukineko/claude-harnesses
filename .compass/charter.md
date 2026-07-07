## north_star
phase-9 cross-task 学習の第2スライス = verified run から教訓を自動 capture する write 側。entry スライス(retrieve: lessons store + condukt Phase-1 注入)は landed 済。今の ONE = gate PASS 後に、完了 run の構造化事実(goal / task titles / done_criteria / verifier(gate) reasons)を決定論コードで harvest し、それを grounding に LLM が再利用可能な教訓1件(kind: error-pattern|convention)を著述して cross-project lessons store へ冪等 append する最小の capture 経路を1本通す。決定論でよい所(いつ harvest するか・どの事実を出すか・冪等性・境界隔離)はコード、意味判断(教訓文の中身)だけ LLM。fugu-router 不在は no-op(soft 依存・後方互換)。これで write→retrieve 往復が end-to-end で閉じる(entry=retrieve, 本スライス=capture)。epic 全体(類似タスクの修正サイクル削減の計測)は後続スライスへ parked。安全 invariant(意味判断は LLM・never-break-a-turn・untrusted 注入は境界隔離)は不変。

## definition_of_done
- condukt lessons harvest --run <RID> が完了 run-state を読み、教訓著述の grounding となる構造化事実を JSON で出す(goal, tasks:[{title, done_criteria}], verifier/gate reasons)。run 不在・空 run は fail-soft(空 JSON を出し非0終了しない)。unit test で seeded run-state から期待フィールドが出ること・不在 run が空を返すことを実証し cargo test green
- condukt SKILL.md Phase 8(クローズ)に、既存 soft 依存群(hypothesis measure / deepwiki 更新 / replay promote)と同一パターンで lesson-capture ステップを追加する: gate PASS 後 fugu-router があれば driver が harvest した事実を grounding に教訓1件(kind error-pattern|convention)を著述し fugu-router lessons add --source-run <RID> で append する。fugu-router バイナリ不在時は no-op で Phase 8 の出力形を一切変えない(後方互換)
- append は content-derived id で冪等(既存 lessons store の保証を再利用)。harvest→add→search の往復 test で『著述した教訓が retrieve され、同一内容の再 add が true no-op(重複追加ゼロ)』を決定論的に実証する
- 純加算で既存挙動を温存: fmt と clippy(-D warnings) clean、condukt(および触った crate)の cargo test green、condukt を lockstep micro bump(SKILL.md + 新 subcommand を触るため Cargo.toml + plugin.json + marketplace.json の3ファイル)。harness-core lessons store は再利用し無改変または純加算。既存 Phase 8 soft-dep 経路は untouched(純加算)

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト

## current_gap
ゴール(verified run の教訓が gate PASS 時に自動で cross-project store へ capture され、次タスクの condukt interpreter へ Phase-1 注入で転移する) − 現状 の最大差分: retrieve 側(lessons store の append/search + condukt Phase-1 lessons_context 注入)は landed 済だが、capture 側が皆無 — 教訓の追加は fugu-router lessons add の手動 CLI のみで、完了 run から教訓を自動抽出する経路が無い(証拠: condukt/src/*.rs に lessons 参照ゼロ; SKILL Phase 8 は hypothesis/deepwiki/replay の soft-dep のみで lesson-capture 無し)。差分を埋める最小 capture 経路 = (1) condukt lessons harvest --run RID で完了 run-state から著述 grounding となる構造化事実を決定論的に emit + (2) SKILL Phase 8 に既存 soft-dep と同型の lesson-capture ステップ(harvest 事実を grounding に教訓1件を著述し fugu add で冪等 append・fugu 不在は no-op)。size m。phase-9 cross-task 学習の第2スライス(write 側)で write→retrieve 往復を end-to-end で閉じる。epic 全体(修正サイクル削減の計測)は parked。

## next_action

## parked

