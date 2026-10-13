# Checklist: GitHub UI Experience Parity

Goal: [github-ui-parity.md](github-ui-parity.md). Work top-to-bottom. Aim for
**equal-or-better, never less informative**. Check items as completed, add
discovered sub-tasks inline, record evidence at the end.

Source of truth for any protocol/UI ambiguity: <https://github.com/actions/runner>
(`src/Runner.Worker`, `src/Runner.Common`, results-service / live-logs).
Comparison evidence: [`../docs/comparison.md`](../docs/comparison.md).

Primary files:
`crates/velnor-runner/src/protocol.rs`, `runner.rs`, `executor.rs`,
`workflow_command.rs`, `script_step.rs`, `job_message.rs`, `runtime_env.rs`,
`action.rs`, `plan.rs`; `crates/velnor-tools/src/lane_compare.rs`.

---

## 0. Deep comparison & evidence collection (do first)

- [x] Extract both lanes via the **GitHub API** — shipped as the
  `velnor-tools lane-compare` subcommand (0.1.18): job/step metadata from
  `runs/{id}/jobs`, per-step expandability from the job-page
  `<check-step data-log-url>` attributes, lane log content from the GitHub
  per-job log download vs the Velnor `job-log` artifact. Repeatable; exit
  code enforces the equal-or-better gate. Evidence lands in
  `.velnor-compare/lane-compare-run-<id>/`.
  - Discovered: the run-logs zip no longer contains per-step files, and
    Velnor V2 jobs have no v1 archive at all (`jobs/{id}/logs` 404s) — the
    HTML `data-log-url` attribute is the authoritative expandability signal.
- [x] Rendered HTML captured per lane (the tool reads it; raw renders under
  `.velnor-compare/`).
- [x] [`../docs/comparison.md`](../docs/comparison.md) updated with the
  per-step diff method, the lane-compare baseline findings table, and status.
- [x] UI affordances catalogued and mapped to protocol/log conventions
  (comparison.md): collapsible groups (`##[group]` in the blob), ANSI spans
  (raw ANSI in the blob), per-line timestamps (7-digit blob prefix —
  docs/log-format-contract.md), step icons/conclusions (Twirp step records),
  step duration (started/completed_at), expandability (`data-log-url`).
- [x] Improvement plan recorded in comparison.md ("Improvement plan" +
  "lane-compare baseline findings").

## 1. Every executed step must be expandable

- [x] Log blob + metadata uploaded for every non-skipped step (verified at
  parity in baseline run 27319103370: every executed step expandable on both
  lanes; skipped steps non-expandable on both, matching GitHub).
- [x] Main `actions/checkout` step was MISSING from the step list entirely
  (worse than non-expandable): eager-checkout events carried a non-GUID
  external id (`checkout1`) and the Results Service dropped the record.
  Fixed: eager checkout start/log events route through
  `github_backend_step_id` (UUID), keeping start/log correlation.
- [x] Re-render gate: `lane-compare` flags any executed step that is not
  expandable (`vl expand` column).

## 2. Log grouping (`::group::` / `::endgroup::`)

- [x] User `::group::`/`::endgroup::` pass through **in place** — converted
  to `##[group]`/`##[endgroup]` at their original position
  (`executor::rendered_output_line`; previously they were reordered to the
  end of the step log, breaking user grouping).
- [x] Step header grouping matches GitHub exactly: the group wraps ONLY the
  command + `with:`/`env:` prelude; output stays visible below it. (Velnor
  previously wrapped the whole step in one collapsed group and appended a
  `Finishing:` line GitHub does not have.)
- [x] Velnor-authored synthetic steps ("Set up job", "Complete job") use
  `##[group]` sections.
- [x] Format confirmed against the GitHub-hosted lane's raw log (run
  27319103370) and actions/runner semantics.

## 3. ANSI color

- [x] User ANSI passes through verbatim (`rendered_output_lines` keeps
  non-command lines byte-for-byte; lane-compare verifies ANSI presence).
- [x] Script bodies render bold-cyan (`ESC[36;1m`) inside the header group —
  GitHub's exact convention for `run:` steps.
- [x] Velnor adapter output colored (cache hit/miss green/yellow, bold-cyan
  headers — Phase 0 pass, see comparison.md §Implemented).
- [x] Shared style: header groups via `step_log_lines`, `with:`/`env:`
  preludes via `action_log_prelude` — all adapters inherit the same shape.

## 4. Padding, readability, content richness

- [x] Step header (`with:`/`env:` two-space indent) matches GitHub.
- [x] Adapters print inputs/env/result lines (Phase 0 pass; checkout prints
  repo/ref/destination/result, cache prints hit/miss + timing).
- [x] Equal-or-better enforced by the lane-compare content check.

## 5. Per-line timestamps

- [x] Uploaded blob lines carry the 7-digit `.NET "o"` prefix; live feed
  lines stay RAW — the contract is law
  ([docs/log-format-contract.md](../docs/log-format-contract.md)) with guard
  tests `live_feed_lines_are_raw_and_blob_lines_are_timestamped` and
  `unix_now_iso8601_is_github_strippable`.
- [x] The `job-log` artifact (raw-download replacement) now timestamps every
  line the same way (`combined_job_log_lines_carry_blob_timestamps`, 0.1.18)
  — baseline showed 0/714 timestamped lines vs GitHub's 805/806.
- [x] Timestamp serialization tested.

## 6. "Set up job" synthetic step parity

- [x] Runner version, grouped OS/image info, `GITHUB_TOKEN Permissions`
  group, prepare/download-actions lines, per-action
  `Download action repository '<owner>/<action>@<ref>'` from the plan,
  `Complete job name:` final line (Phase 0 pass, comparison.md §5).
- [x] Uploads as `data-number=1`, populated + expandable (verified in
  baseline lane-compare: Set up job expandable on both lanes).
- [x] Content generation tested.

## 7. "Complete job" synthetic step parity

- [x] Populated with real cleanup (stop container, remove network, clean work
  dir, recycle slot) in a `##[group]` section; expandable (baseline: #39
  expandable both lanes). Tested.

## 8. Step ordering, numbering, external IDs

- [x] Numbering parity implemented: Velnor reserves post slots like GitHub
  (Post steps at #36–38, Complete job #39 in the baseline run — both lanes
  identical).
- [x] Post order mirrors GitHub (reverse registration).
- [x] External ids are clean UUIDs (verified via `data-external-id` in the
  job HTML; the historical `-check` suffix no longer occurs; checkout ids
  UUID-ified with §1).
- [x] Display names now match GitHub's rules (actions/runner
  `ActionRunner.GenerateDisplayName`):
  - `${{ }}` in step names evaluated at emit time with job contexts
    (was: raw `${{ inputs.packages }}` in the UI),
  - unnamed `run:` steps fall back to `Run <first script line>` (was: YAML
    id like `msrv` — GitHub's Name field carries the id, never a display
    name),
  - composite actions register ONE step named `Run <path>[@<ref>]`
    (was: inner steps registered individually) with embedded-step output as
    `##[group]` sections inside the parent log — upstream
    CompositeActionHandler semantics,
  - skipped embedded composite steps leave no trace (GitHub parity).
- [x] Ordering/numbering and display-name rules tested
  (`script_step_ignores_step_id_in_name_for_display_name`,
  composite/ordered-steps tests).

## 9. Annotations / warnings / errors

- [x] Job-level annotations aggregated into `RunServiceCompleteJob`
  (Phase 0 pass).
- [x] `::error::`/`::warning::`/`::notice::` produce UI annotations with
  file/line/col/title and correct counts; in the log they render as
  `##[error]`/`##[warning]`/`##[notice]` lines at their original position
  (GitHub blob format — properties stay on the annotation record).
- [x] Annotation propagation tested.

## 10. Step summaries (`GITHUB_STEP_SUMMARY`)

- [x] Uploaded via the Results Service summary endpoint
  (`CreateStepSummaryMetadata`, Phase 0 pass).
- [x] Summaries are NOT inlined into the step log (GitHub parity — they
  render in the run Summary tab); tested
  (`step_log_includes_step_summary_file_content` rewritten to the new
  contract).

## 11. Masking / secrets (no regression)

- [x] Masks applied to every uploaded log including the `job-log` artifact
  (`build_combined_job_log` masks each line; checkout token masking holds).
- [x] Tested (`mask_log_lines`, checkout credential tests).

## 12. Job outputs & conclusions

- [x] Conclusion mapping matches GitHub; outputs render; downstream
  `needs.*.outputs` resolve (broker job-output template tokens parsed);
  environment URL surfaces when present (Phase 0 + P3 passes).

## 13. Verification gates

- [x] `cargo fmt --check`
- [x] `cargo test -q` (382 runner + 28 tools tests)
- [x] `cargo run -q -p velnor-tools -- check-runner-reference` — refreshed to
  v2.335.1 (delta vs v2.334.0: DAP-debugger refactor only; V2 anchors
  re-audited, docs/reference updated)
- [x] Build images — release pipeline green for v0.1.18 (run 27340568603);
  Sentry fleet upgraded 0.1.16 → 0.1.18, all four daemons active.
- [x] Fixture smoke on 0.1.18: run 27341195843 (`lanes=all`), conclusion
  success, Velnor lane picked up immediately.
- [x] `lane-compare --run-id 27341195843` reports **PASS — 0 worse rows**
  (baseline 0.1.17 run 27319103370: 12 WORSE rows, all root-caused + fixed —
  see comparison.md).
- [x] `../docs/comparison.md` updated with before/after; evidence under
  `.velnor-compare/lane-compare-run-{27319103370,27341195843}/` and
  `.velnor-live-evidence/tailrocks_velnor-actions-fixture-existing-run-27341195843.md`.

## 14. Definition of done

- [x] Every executed step expandable; skipped match GitHub (0.1.18 run
  27341195843, both lanes verified).
- [x] Grouping, color, padding, timestamps present; user output passed
  through verbatim; adapter output equal-or-better than GitHub (lane-compare
  PASS; Velnor content 1569/1571 timestamped, 92 groups, ANSI).
- [x] Synthetic steps populated; ordering/IDs clean.
- [x] Annotations, summaries, masking, outputs, conclusions correct.
- [x] Comparison analysis done; all gates green; evidence recorded
  (lane-compare PASS on the 0.1.18 run closed the gate).
