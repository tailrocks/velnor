# G0 lane-compare exact-source review

Date: 2026-09-20

## Revision and disposition

- Reviewed exact detached checkout `/private/tmp/g1-lane-compare-review-3ad`.
- `HEAD`: `3ad452acd8004c06cb866f75392bb5749adfe818`.
- Parent/base: `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`.
- Worktree was detached and clean. No implementation files, branches, or hosted state were changed.
- Verdict: **REJECT SOURCE** pending the three fail-closed fixes below. This is source review only; it makes no hosted-check or merge decision.

## Verified repaired paths

The intended repairs are present:

- Explicit selectors are documented as diagnostic subsets, while `lane_compare` fetches the complete jobs census and validates it before `select_pairs`; selected IDs must be an already validated pair. See `crates/velnor-tools/src/lane_compare.rs:53-62,221-241,253-300,329-342`.
- Full-run and subset decisions are distinct. A selected subset reports `DIAGNOSTIC ONLY` and exits nonzero; a complete strict comparison is labeled `AUXILIARY PASS`, with an explicit no-authoritative-identity limitation. See `:201-219,403-439`.
- Empty/control-only workloads and orphan/duplicate/ambiguous/skipped census rows fail before evidence comparison. See `:661-672` and `:1198-1272`.
- Run and matched-job terminal success are required; missing conclusions, failed/cancelled/timed-out/skipped/nonterminal jobs fail. See `:634-695,704-829`.
- GitHub HTML/log and Velnor artifact acquisition is no longer warning-to-empty. Empty maps/text and missing artifacts fail. See `:736-758,767-829,848-885,1001-1073`.
- Jobs and artifacts use complete pagination, stable totals, duplicate detection, truncation detection, and required artifact names. See `:578-619,939-998,1009-1038`.
- Unexpected Velnor-only logical steps increment parity failures. See `:1952-1963`. Watch routes every sampled run through the same census/evidence/timing validator. See `:474-484,1495-1543`.
- Baseline samples reject an empty or changing baseline class set. See `:1545-1593`.

## Blocker 1 — nonempty but partial HTML can pass

`fetch_job_html_steps` rejects only an entirely empty parsed map (`:879-884`), and `assess_run_evidence` repeats only the map-nonempty check (`:736-741`). It never requires every non-skipped job step number to occur exactly once in the HTML evidence. `step_verdict` reports missing HTML only when GitHub has `Some(true)` and Velnor has `None`; when both maps omit the same executed step, it returns `ok` (`:1856-1868`).

Hostile fixture used against this exact binary:

```text
env PATH=/private/tmp/g1-lane-fixtures/bin-partialhtml-new:$PATH \
  target/debug/velnor-tools compare --repo tailrocks/velnor --run-id 44 \
  --output-dir /private/tmp/g1-lane-fixtures/out/new-partial-html --strict true
```

The fake jobs each contain successful steps 1 and 2; fake HTML contains only a nonempty `<check-step data-number="1">`. The command exited `0` and emitted `AUXILIARY PASS`; report:
`/private/tmp/g1-lane-fixtures/out/new-partial-html/lane-compare-run-44/report.md`.
The report shows step `2/2` with `gh expand ?`, `vl expand ?`, verdict `ok`. A truncated or malformed page can therefore erase the only UI-parity signal for an executed step. Required behavior is fail-closed (`NOT_PROVEN`/failure) unless HTML covers all job steps, with duplicate/invalid numbers rejected.

## Blocker 2 — watch accepts current workload drift against a complete baseline

`baseline_from_samples` checks only the baseline slice (`:1549-1557`). `is_regression` iterates current classes and silently skips any class absent from the baseline (`:1469-1480`); it never requires `current.jobs.keys()` to equal `baseline.jobs.keys()`. The watch report also renders only current classes (`:1618-1639`).

Hostile baseline contrast used against this exact binary:

```text
env PATH=/private/tmp/g1-lane-fixtures/bin-watch-drift-new:$PATH \
  target/debug/velnor-tools compare --repo tailrocks/velnor --workflow compat.yml \
  --watch --since 2 --regress-threshold 0 \
  --output-dir /private/tmp/g1-lane-fixtures/out/new-watch-drift
```

The current successful run has only class `app-a`; the sole baseline run has `app-a` and `app-b`, with complete successful jobs, logs, HTML, artifacts, and timing. The command exited `0`, reported `AUXILIARY PASS`, and rendered only `app-a`:
`/private/tmp/g1-lane-fixtures/out/new-watch-drift/lane-compare-watch/report.md`.
This hides a missing current workload class instead of returning `NOT_PROVEN`/failure, violating the full-unit baseline contract.

## Blocker 3 — watch silently drops failed recent runs

`recent_completed_run_ids` filters the `gh run list` result to successful conclusions before any run is validated (`:514-549`). If a failed recent run is followed by two older successful runs, `--since 3` produces two samples and the watch proceeds as if the failed run did not exist. A failed required run should invalidate the sample or make the watch `NOT_PROVEN`, not disappear from the selected window.

Hostile fixture used against this exact binary:

```text
env PATH=/private/tmp/g1-lane-fixtures/bin-watch-failed-new:$PATH \
  target/debug/velnor-tools compare --repo tailrocks/velnor --workflow compat.yml \
  --watch --since 3 --regress-threshold 0 \
  --output-dir /private/tmp/g1-lane-fixtures/out/new-watch-failed
```

The fake list is `[46 completed/failure, 44 completed/success, 45 completed/success]`; run 46 is ignored, runs 44/45 are sampled, and the command exits `0` with `AUXILIARY PASS`. Report:
`/private/tmp/g1-lane-fixtures/out/new-watch-failed/lane-compare-watch/report.md`.

## Independent checks

- `rtk cargo test -p velnor-tools --all-targets` → `218 passed`.
- `rtk cargo clippy -p velnor-tools --all-targets --locked -- -D warnings` → no issues.
- `rtk cargo test -p velnor-tools lane_compare -- --nocapture` → `41 passed`.
- `rtk git diff --check abe9ad82a2d4d01b706bbc6122ab6ccb150faad9..3ad452acd8004c06cb866f75392bb5749adfe818` → clean.
- Explicit selector + extra unpaired job (`run 42`, selectors `4201/4202`) exited `1` before evidence with the complete-census error; valid full run `44` exited `0` with `AUXILIARY PASS` and selector run `44` exited `1` with `DIAGNOSTIC ONLY`.
- Missing HTML, missing GitHub log, missing Velnor artifact, and empty Velnor artifact hostile fixtures each exited `1` with an evidence error.
- Existing baseline negative tests pass: `baseline_from_samples_rejects_samples_without_usable_timing` and `baseline_from_samples_rejects_partial_timing_coverage` (`:2933-2945`). They do not cover current-vs-baseline class drift above.

## Identity scope

The source correctly says lane-compare is auxiliary and does not prove source/ref, checkout SHA, required-check association, provider, runner, host, or trust identity (`:323-326`, `:1657-1666`). This review makes no authoritative gate claim; the rejection is solely for the three local fail-closed decision paths above.
