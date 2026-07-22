## north_star
plugin rollout drift(committed+version-bumpされた修正が実際のplugin cacheへrolloutされないまま残る状態)を、advisoryな警告ではなく機械的なblockingゲートで防ぐ。根拠(2026-07-22、当セッション実測): scripts/check-plugin-rollout.pyは.githooks/pre-pushに既に組み込まれているが、そのヘッダコメントで「ADVISORY-ONLY」と明記され、drift検出時(exit 1)も「This is advisory only — the push is not blocked.」と出すだけでpushを止めない設計になっている。さらにこのローカルcloneではcore.hooksPathが未設定(空文字)で、pre-push自体が発火していなかった。結果、当セッションだけでautoflow・budgetguard・compass・condukt・ctxrot・deepwiki・difflog・fugu-router・gauge・harness-status・playbook・runbook・session-insights・ship・tdd・tracekitの16pluginが、commit・version bump済みにもかかわらずrolloutされないまま約20コミット積み上がった(手動でcheck-plugin-rollout.pyを実行するまで誰にも検知されなかった)。fail-openのenforceをGitHub依存からlocal pre-commitへ移した今セッションの方針(判定不能はblockに倒す・外部サービスに預けない)を、version drift(判定不能ではなく既に判定済みのdriftを握り潰す設計)にも適用する。

## definition_of_done
- pre-pushのrollout-drift検査(scripts/check-plugin-rollout.pyのexit 1、「committed but not rolled out」サブクラス)が、pushをblockするよう動作を変更済み。使い捨てworktree等で、意図的にdriftを作った状態からのgit pushが非0で拒否されることを実測で確認する。
- このrepoのローカルcloneでcore.hooksPathが.githooksに設定済みであり、pre-pushフックが実際に発火することをgit config core.hooksPathの出力で確認する。
- pre-pushの他の3チェック(GATE crate示唆・autonomy chain示唆・chronically-red CI)と、scripts/check-plugin-rollout.pyの他の2サブクラス(exit 2 enablement・exit 3 unverifiable)は既存どおりadvisoryのまま維持し、今回blockingに変えるのはrollout-drift本体(exit 1)のみであることをdiffで確認する(スコープを広げすぎない)。
- scripts/check-plugin-rollout.pyのヘッダコメント、または.githooks/pre-push側のコメントで、rollout drift検出時にpre-pushへ非0を返してpushを止める契約が明文化されている(スクリプトのコメント/ドキュメントが実装と一致)。

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト

## current_gap
pre-pushのrollout-drift検査は既に実装されているが、exit 1でも常にpushを許可する設計になっており(class 2以外の3クラスは意図的にadvisoryのまま据え置く)、かつこのcloneではcore.hooksPathが未設定でそもそも発火していない。この2点を埋めるのが最大の差分。

## next_action
.githooks/pre-pushのrollout drift分岐(rc=1)でexit 1して以降の処理を止めるよう変更し、このcloneでgit config core.hooksPath .githooksを実行し、使い捨てworktreeでdrift状態からのpushが拒否されることを確認する。

## parked
- 旧DoD2 GitHub required status checkでmergeをブロックする件: 方針7により機構撤去。CI rulesetはadvisoryへ降格し、enforceはlocal pre-commitへ移管済み。再登録は方針7に反するため意図的に行わない。
- 旧north_star 出荷済み並列衝突ハードニングのvalidate閉環。overwatchはdrift無しでlive。残るのはruntime-conflict merge-hold contended-skipの時間窓集計surfaceとbefore-after deltaのevidence化。
- specguard・stuckguard・overwatch の harness_core::verdict 非適合判定(2026-07-22): 型を無理に統合するのではなく、必要なら harness_core::verdict::Determination<T> 等の別の共有型で個別に判断する。今は着手しない。
- [達成 2026-07-22] fail-openゲート機構の型・enforce・host非依存化(旧north_star)。DoD1は構造適合6crateで完了、check-fail-open --allは0件、enforceはlocal pre-commit。詳細はgit log 43ce376a周辺を参照。

