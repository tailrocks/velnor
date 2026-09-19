# G0 lane-compare exact-source review

Date: 2026-09-20

## Revision and disposition

- Reviewed exact detached checkout `/private/tmp/g1-lane-compare-review-627`.
- `HEAD`: `627c36f592437e4aef5f0d7d7f19ec85d391bac3`.
- Parent/base: `3ad452acd8004c06cb866f75392bb5749adfe818`.
- Worktree was detached and clean. No implementation files, branches, or hosted state were changed.
- Verdict: **APPROVE SOURCE** for the three fail-closed blockers from `review-3ad452acd.md`.
- This remains an auxiliary comparison tool; its `AUXILIARY PASS` does not prove source/ref, checkout SHA, required-check association, provider, runner, host, or trust identity. The independent checker remains authoritative.

## Hostile CLI verification

All commands used the built `target/debug/velnor-tools` from this exact checkout and disposable fake `gh`/`curl` fixtures under `/private/tmp/g1-lane-fixtures`.

| Case | Result |
| --- | --- |
| Nonempty partial HTML, run 44 | Exit `1`; rejects missing executed step `2 (Run security)` before comparison. |
| Watch current/baseline workload drift | Exit `1`; report is `NOT PROVEN` with workload-set drift (`1` vs `2` units). |
| Watch recent failed run | Exit `1`; rejects run `46` (`completed/failure`) instead of filtering it out. |
| Explicit selectors plus unpaired required unit, run 42 | Exit `1`; full census rejects orphan before selected-pair evidence. |
| Valid full run 44 | Exit `0`; explicitly labeled `AUXILIARY PASS`. |
| Valid selectors 4401/4402 | Exit `1`; report is `DIAGNOSTIC ONLY`, never a gate pass. |
| Wrong selector 4401/4499 | Exit `1`; selector is not a validated pair. |
| Missing HTML / GitHub log / Velnor artifact / empty artifact | Exit `1` for each; no warning-to-empty fallback. |

## Regression and static checks

- `rtk cargo test -p velnor-tools lane_compare -- --nocapture`: **44 passed**.
- `rtk cargo test -p velnor-tools --all-targets`: **221 passed**.
- `rtk cargo clippy -p velnor-tools --all-targets --locked -- -D warnings`: **pass**.
- `rtk cargo fmt --all -- --check`: **pass**.
- `rtk git diff --check 3ad452acd..627c36f5`: **pass**.
- Artifact page-2 pagination test: **pass**.
- Artifact truncation/duplicate/absence/API-error test: **pass**.
- Isolated current `workflow_command_parse_benchmark`: **pass** (`9.98s`). Clean-base `3ad452acd` isolated benchmark: **pass** (`5.70s`).

## Full-workspace check caveat

`rtk cargo test --workspace --all-targets` exited `101`: `2337 passed, 1 failed, 5 ignored`. The only failure was unrelated `velnor-runner::workflow_command::tests::workflow_command_parse_benchmark`, measured at `19.467662125s` against its 10-second threshold while many repository-wide cargo/rustc jobs held the shared mbx target/fleet resources. The same benchmark passed in isolation on both current and clean-base checkouts. No claim that the workspace failure is preexisting is made; full clean-base workspace reproduction was not run.

No source mutation or merge was performed by this review.
