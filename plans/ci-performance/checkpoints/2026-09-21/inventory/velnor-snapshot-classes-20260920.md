# Velnor retained CI class inventory (snapshot 2026-09-20)

Snapshot collected at `2026-09-20T16:49:52.946185+00:00`. This is a read-only audit of saved artifacts; no GitHub/network calls or repository changes were made.

## Coverage reconciliation

- Runs: 7,594 rows and 7,594 distinct IDs. The source summary reports 76 unfiltered pages and API `total_count=7,594`; the saved compact index matches both numbers. Oldest creation `2026-06-04T22:42:50Z`; newest `2026-09-20T16:23:53Z`. This covers the retained response only, not Velnor's entire history.
- Earlier attempts: 268 records across 182 rerun runs. Expected attempt keys from each latest `run_attempt` reconcile: 268 expected, 268 retained. Outcomes: `{"cancelled": 155, "failure": 105, "null": 2, "startup_failure": 1, "success": 5}`; two historical attempt rows remain queued in the API snapshot.
- The non-green ledger contains 3,901 rows and reconciles to 3,638 latest non-success runs plus 263 non-success earlier attempts.
- Main-branch push sample: 766/1565 first attempts green, 770/1565 latest attempts green; 21 runs had retries. Denominator is workflow runs, not per-commit aggregate pipelines; cancellations and historical workflows are included.
- The summary reports `filter=all` job collection for 559 runs and 17,961 unique jobs, but the raw all-jobs inventory is not present here, so those two figures cannot be independently recomputed. The saved failed/nonterminal/cancelled ledger does reconcile: 1,651 rows, 1,651 unique job IDs across 411 runs and 573 run-attempt pairs. Outcomes: `{"cancelled": 661, "failure": 989, "null": 1}`.

## Evidence-backed classes and snapshot disposition

Counts are ledger rows, not unique incidents. Combined labels remain one row each; the full occurrence-ID arrays are in the companion JSON. Statuses below are as of the snapshot time, not a live 2026-09-21 refresh.

| Class | Rows | Representative run/attempt:job | Evidence and status at snapshot |
| --- | ---: | --- | --- |
| `generated-tree;candidate-product` | 58 | 35479860264/1:105995596023, 35474432949/1:105981120289, … | Renderer/pin/scan-input identity mismatch plus missing event-equivalent, source-bound candidate producer. Both defects remain open in this snapshot. |
| `generated-tree` | 28 | 35467711578/1:105962985201, 35503134927/1:106058335427, … | Renderer/pin/scan-input identity mismatch. Main and Preview examples remain open; exact source-bound render parity is still required. |
| `candidate-product` | 27 | 35475034188/1:105982708614, 35495039936/1:106036394460, … | Candidate lookup lacks an event-equivalent source-bound producer. Main-only producer lookup remains open. |
| `dead-code` | 6 | 35520025700/1:106102422339, 35520025700/1:106102422370, … | Stale GuardError::Contended use after acquisition semantics changed. Fixed at 7308307b; successor PR 35522028476 is green, but main CI proof was pending. |
| `superseded-diagnostics-deletion;test-failure` | 1 | 35520025700/1:106102422435 | Historical candidate deleted runner state before an unchanged test asserted diagnostics survived. Current main omits that candidate deletion; the test passed in successor PR 35522028476. This occurrence does not establish a current main defect. |
| `protocol-expectation;test-failure` | 1 | 35521293443/1:106105813547 | Stale expectation for RunnerNotFound versus NotFound. Fixed at c24be56e; successor PR 35522028476 is green, but main CI proof was pending. |
| `dirty-preview` | 4 | 35468978367/1:105968085418, 35504500719/1:106063528303, … | Generated metadata dirtied an identity-sensitive source checkout. Repair 57e7cafc moves it to runner.temp; the successor Preview was blocked at policy, so end-to-end repair proof was absent. |
| `cross-compiler` | 4 | 35468978367/1:105968085428, 35504500719/1:106063528246, … | ARM preview requested aarch64-linux-gnu-gcc on an x64 Ubuntu runner. The current route remained wrong; open PR 962 was relevant. |
| `test-failure` | 48 | 34718367936/1:103619583776, 35057652729/4:104674700366, … | Test failure with root cause still unclassified. Successor/source inspection is required for each occurrence. |
| `rust-lint` | 18 | 32820212810/1:97717465757, 32820212810/2:97718872267, … | Rust compile/lint failure with individual diagnostics retained. No blanket repair or successor proof is established. |
| `missing-event-fields;rust-lint` | 5 | 35516406937/1:106093038186, 35516406937/1:106093038219, … | Event schema expansion was not propagated to test consumers. Historical PR revisions need successor/source inspection. |
| `packaging` | 8 | 32818378790/1:97712015110, 32830311665/1:97748833843, … | Artifact construction/identity mismatch. Historical publication/package failures still need current-invariant and successor verification. |
| `lockfile` | 1 | 32822578080/1:97723705868 | Tracked lockfile was inconsistent with build inputs. Historical occurrence; successor verification is absent. |
| `unavailable-log` | 43 | 34914871725/1:104210220726, 32891472827/1:97946004707, … | Job log API returned 404; cause is unproven. Missing log is an evidence gap, not a solved or benign failure. |
| `unclassified` | 1399 | 35479860264/1:105997417384, 35479860264/1:105997431546, … | No root-cause disposition. This label includes failed and cancelled jobs; cancellation intent is not inferred. |

The renderer/candidate-bootstrap groups contain 113 distinct ledger rows: 58 with both labels, 28 renderer-only, and 27 candidate-only. Do not add the two cause totals without preserving the overlap.

## Evidence gaps and limits

- Exact excerpts cover 790 of 1,651 ledger rows: 789 failed jobs plus one null/in-progress job. 200 failed rows have no excerpt; of those, 43 are explicitly marked API 404, 149 retain only a log SHA-256, and 8 have neither excerpt nor SHA-256. The 661 cancelled rows also have no excerpt. A hash proves identity only, not the missing log contents.
- The CSV's `raw_log` fields point to `/tmp/velnor-failures/logs`; that directory is absent now (0 of 1,651 referenced paths exist). The snapshot contains 12 compressed `.log.gz` files; 11 match ledger job IDs, and those IDs are already among the excerpted set. The manifest's 28 file hashes all verify (28/28).
- The 1,399 `unclassified` job rows comprise 738 failures and 661 cancellations. None has a causal disposition; cancellation intent is not inferred. `unavailable-log` adds 43 more failures with explicit API 404 but no causal evidence.
- The snapshot records older missing job logs as API 404/410, but it does not preserve a raw all-jobs response set. The summary's 559/17,961 jobs-query totals are therefore reported metadata rather than independently reproducible evidence here.
- The snapshot was collected 2026-09-20. It is not a live status refresh. PR-success evidence for `dead-code` and `protocol-expectation` does not establish mainline success; the exact main proof remained pending in the saved disposition.

## Separate Jackin history context

- Jackin run 35521080097/job 106105226160 now has an extracted local raw log. It confirms `conformance_partial_success_is_not_retried` failed with assertion left 7/right 1; the test summary was 107 passed, 1 failed, 1 skipped, with 14/122 unrun after fail-fast. This is a Jackin test failure and is not evidence for any Velnor class.
- Jackin run 35515575859/job 106090835001 has an explicit higher-priority waiter annotation for the same main concurrency group. That confirms the cancellation cause for this one job only; it does not classify Velnor's 661 cancelled ledger rows.

## Audit artifacts

- Machine-readable counts and per-occurrence IDs: `/Users/donbeave/Projects/work/ci-evidence/inventory/velnor-snapshot-classes-20260920.json`
- Reusable local analyzer: `/Users/donbeave/Projects/work/ci-evidence/inventory/analyze_velnor_snapshot.py`
- Source ledger: `/Users/donbeave/Projects/work/velnor/plans/ci-performance/observations/reliability-20260920/inspected-failed-jobs.csv.gz`
- Source excerpts: `/Users/donbeave/Projects/work/velnor/plans/ci-performance/observations/reliability-20260920/failure-excerpts.md.gz`
- Summary: `/Users/donbeave/Projects/work/velnor/plans/ci-performance/observations/reliability-20260920/inventory-summary.md`
