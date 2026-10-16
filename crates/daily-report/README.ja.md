- session-insights の record ノートは書き換えないこと。
- 数値を捏造しないこと。
- `<vault>/daily/<date>.md` 以外には書かないこと。[test: scripts/test_prose_daily_report.py::test_claim_write_only_daily]
- `records/` には決して書き込まないこと。[test: scripts/test_prose_daily_report.py::test_claim_write_only_daily]
- 既存の手書き日報は上書きしないこと。

サブスクリプション完結（skill のみ・バイナリ無し・API キー不要）。[test: scripts/test_prose_daily_report.py::test_claim_skill_only_layout]
