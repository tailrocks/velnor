# Jackin bootstrap and critical-path audit

Audit baseline: Jackin PR1007 source `f4054488919e3267bfe2eef2d7d92ada25e288f5`, main `fce94cea8a15de0c2db3bb4ff880d741baf5c00a`, exact desktop cancellation run 35515575859/job 106090835001, and latest main desktop run 35521079960/job 106105155956. This records observations and bounded fixes; it does not claim the 120-second target is met.

## Findings

The exact cancelled desktop job took 35m28s (workflow elapsed 35m36s). “Set up Mise tools” took 17s; “Run desktop-merge” then occupied 34m57s before cancellation. GitHub’s annotation gives the cause: a higher-priority waiter existed in `desktop-merge-jackin-project/jackin-refs/heads/main`. The generated workflow uses that same repository/ref group with `cancel-in-progress: true`, so a later main push can cancel a distinct main candidate. The annotation and job summary are visible on [run 35515575859/job 106090835001](https://github.com/jackin-project/jackin/actions/runs/35515575859/job/106090835001).

At the inspected PR1007 revision, the desktop profile passes a limited `install_args` list to `mise-action`, then runs `mise run desktop-merge` without disabling task-time auto-install. The task graph is `desktop-merge` → `desktop-ci` plus `desktop-test-ui`; `desktop-ci` covers code generation, Swift formatting/lint, Rust-backed desktop tests, build, and verification. A later task can therefore activate configured tools outside the initial list. The source-install durations below came from the prepared exact-run observations, not a newly retrieved raw log; do not sum them because work overlaps.

| Source-built tool reported in job 106090835001 | Reported time |
| --- | ---: |
| `cargo-audit` | 9m20s |
| `codebook-lsp` | 9m03s |
| `cargo-dylint` | 7m21s |
| `dylint-link` | 2m55s |

The task graph also runs deterministic UI tests after merge. `native/Scripts/run-ui-tests.sh` invokes `xcodebuild test` serially for each test method. The generated desktop workflow is triggered by main push/manual dispatch, so deterministic UI failures can first appear after merge. The separate release workflow runs release environment/state preparation and builds/verifies; its tag path additionally signs, notarizes, and attests. No current main/tag release-run timing sample was available, so release remains an unmeasured, in-scope timing class.

## Elapsed-time evidence

| Class and source | Elapsed | Status and limit |
| --- | ---: | --- |
| Cancelled desktop main, run 35515575859 | 35m36s workflow; 35m28s job | Cancelled by same-ref concurrency; narrow setup 17s, task step 34m57s. |
| Latest inspected main desktop, run 35521079960 | 41m41s workflow; 41m29s job | Success on `fce94ce`; tools setup 3m40s and task step 37m34s from job-step metadata. [Run page](https://github.com/jackin-project/jackin/actions/runs/35521079960). |
| Latest successful same-SHA main pair, `a5e1022` | 34m11s critical path | Desktop completed after the concurrent CI workflow; CI 15m03s, desktop 34m11s. |
| Latest inspected main CI, run 35521080097 | 16m46s | Failed the Rust diagnostics test; not a formatting/Clippy observation. |
| Latest inspected PR workflow, run 35519793543 | 19m48s | Success; exceeds 120s. |
| Release/tag path | No usable current sample | Must be measured; not exempt from 120s. |

Every measured complete workflow breaches 120s by a wide margin. The newest main desktop job is about 20.7× the limit; latest successful full main pair’s critical path is about 17.1×. Workflow/job elapsed values are wall time; step durations are explanatory and must not be added to workflow duration.

## Bounded fixes that preserve coverage

1. In the shared profile generator, resolve task/tool transitive closure, install only that locked closure, and disable every Mise auto-install fallback during task execution. Apply this to desktop, scheduled, and release task jobs; retain macOS-only tools such as Periphery only on macOS and keep Linux/other supported paths intact. Upstream PR978 contains the generic closure direction but does not by itself change Jackin’s desktop/release workflow contract.
2. Pin the Mise runtime used by `mise-action`. Give tool caches keys based on the pinned Mise version, platform/architecture, lock state, and exact profile closure; avoid invalidating every tool cache for unrelated repository configuration edits. Measure restore/upload cost and cold-cache behavior.
3. Keep PR, integration-candidate, and main validation complete and independent. Move deterministic desktop/UI coverage into the merge-candidate required gate; retain only justified post-merge verification. Split the monolithic desktop task into visible phases so setup, generation, build, unit tests, UI tests, and verify time are independently attributable.
4. Fix concurrency in the generator: use stable per-PR/per-workflow cancellation for PR updates; give each main/integration run a unique run-ID group so one revision cannot cancel or replace another. Preserve narrow locks only for stateful publication.
5. Measure release, scheduled macOS, warm/cold, and changed-tool/lockfile runs separately. Keep UI, packaging, signing, notarization, and platform coverage when restructuring; do not treat absent evidence as a pass.

## Evidence limits

The public run/job pages expose annotations and elapsed summaries but require sign-in to view raw logs. Authenticated `gh` log/run detail calls also encountered endpoint-specific 403 rate-limit responses, despite a separate rate-limit query reporting remaining core quota. Thus the cancellation reason, job/run elapsed, and setup/task boundaries have independent GitHub evidence; source-build tool timings are retained from the prepared exact-run observations and were not independently re-read from raw logs. Scheduled desktop showed no run history at inspection. No performance claim is inferred from cache hits alone.

Relevant source at the baseline: [desktop merge workflow](https://github.com/jackin-project/jackin/blob/f4054488919e3267bfe2eef2d7d92ada25e288f5/.github/workflows/desktop-merge.yml), [desktop task graph](https://github.com/jackin-project/jackin/blob/f4054488919e3267bfe2eef2d7d92ada25e288f5/mise.toml), [release workflow](https://github.com/jackin-project/jackin/blob/f4054488919e3267bfe2eef2d7d92ada25e288f5/.github/workflows/release.yml), and [GitHub concurrency semantics](https://docs.github.com/en/actions/how-tos/write-workflows/choose-when-workflows-run/control-workflow-concurrency). GitHub keeps one pending run per concurrency group by default and replaces that pending run when another arrives; `cancel-in-progress: false` alone does not preserve multiple candidates.
