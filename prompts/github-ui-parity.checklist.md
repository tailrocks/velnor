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
`action.rs`, `plan.rs`.

---

## 0. Deep comparison & evidence collection (do first)

The objective starts as an analysis: understand *exactly* how GitHub's UX is
built and where Velnor falls short, then improve.

- [ ] Extract both lanes via the **GitHub API** (more accurate than HTML
  scraping) — job/step metadata and raw log archives. See the commands in
  [`comparison.md`](../docs/comparison.md). Prefer a Rust
  `velnor-tools` subcommand that fetches + diffs the two jobs (mission:
  Rust-first automation, repeatable comparison).
- [ ] Also save the rendered HTML of both lanes under `.velnor-compare/` for the
  visual affordances (groups, colors, padding).
- [ ] Update / extend [`../docs/comparison.md`](../docs/comparison.md)
  with a step-by-step diff: for each step, note expandability, presence of log,
  timestamps, grouping, color, padding, and content richness.
- [ ] Catalogue every UI affordance GitHub uses and whether Velnor emits it:
  - [ ] collapsible groups (`<details class="js-checks-log-group">` ← `::group::`)
  - [ ] ANSI color spans (`ansifg-*`, `ansibg-*` ← ANSI escapes in the log)
  - [ ] per-line timestamps (`CheckStep-line-timestamp`)
  - [ ] line anchors / numbering
  - [ ] step icons & conclusions (success/skip/failure/cancelled)
  - [ ] hyperlinked URLs in log text
  - [ ] step duration display
- [ ] Map each affordance to the protocol field / log-content convention that
  drives it (confirm against `actions/runner`).
- [ ] Produce an "improvement plan" section in `comparison.md`: what to match,
  what to improve beyond GitHub, and how — given Velnor authors its own output.

## 1. Every executed step must be expandable

Observed: Velnor leaves `data-log-url=""` on `actions/checkout` (#2), `sccache`
(#7), some `set -euo pipefail` (#10, #18), `write-result.py` (#19), Post steps
(#23, #24), `Complete job` (#25). GitHub uploads a log for **every** step.

- [ ] Locate where blob upload is skipped when no lines were captured
  (`runner.rs:1799-1809`, `protocol.rs:2500-2601`).
- [ ] Upload a log blob + `CreateStepLogsMetadata` for **every** non-skipped
  step (even empty body) so it gets a `data-log-url` and is expandable.
- [ ] Ensure native-adapter steps route captured output into `StepLog.lines`
  (`executor.rs:178-192`) — they should rarely be empty.
- [ ] Match GitHub on skipped steps: they stay non-expandable (fixture #3).
- [ ] Re-render: no executed step has an empty `data-log-url`.

## 2. Log grouping (`::group::` / `::endgroup::`)

GitHub's "Set up job" and many steps use collapsible groups (rendered as
`<details class="js-checks-log-group">`). Velnor emits none.

- [ ] Confirm how `::group::` / `::endgroup::` workflow commands are parsed
  (`workflow_command.rs`) and whether they survive into the uploaded log.
- [ ] Ensure grouping markers in **user** output pass through and render as
  collapsible sections (verbatim — do not strip).
- [ ] In **Velnor-authored** adapter output, emit `::group::` sections to make
  long output collapsible and scannable (e.g. group apt/install noise, group
  cache key computation). At least as organized as GitHub; ideally cleaner.
- [ ] Verify the Results Service log format GitHub expects for groups (confirm
  in `actions/runner`).
- [ ] Re-render: groups are collapsible in the UI.

## 3. ANSI color

GitHub renders ANSI color (e.g. mise output shows `ansifg-m`, `ansifg-c`). Velnor
must preserve user color and add tasteful color to its own adapter output.

- [ ] Confirm Velnor does not strip ANSI from **user-command** output — colors
  pass through verbatim into the uploaded log.
- [ ] Verify the Results Service log format carries ANSI so GitHub renders color.
- [ ] Add purposeful ANSI color to **Velnor-authored** adapter output (headers,
  success/warn/error markers, key/value emphasis) — informative, not noisy.
- [ ] Establish a small shared style convention for adapter output (a helper for
  group headers, info/warn/error coloring) so all adapters look consistent.
- [ ] Re-render: Velnor output is colored and consistent, never plainer than the
  hosted lane.

## 4. Padding, readability, content richness

GitHub output has consistent indentation/padding and is easy to scan.

- [ ] Audit Velnor adapter output for padding/alignment consistency vs GitHub.
- [ ] Ensure each adapter prints enough context to debug (what it did, key
  inputs, cache hit/miss, paths, versions) — never terser than GitHub's
  equivalent step.
- [ ] Where helpful, improve on GitHub: clearer section headers, summarized
  key→value lines, explicit "result" lines. Keep it honest and accurate.

## 5. Per-line timestamps

Observed: Velnor lines lack `CheckStep-line-timestamp`; GitHub shows RFC1123 GMT
per line; the "Show timestamps" toggle is empty on Velnor.

- [ ] Confirm the timestamp format/placement the Results Service expects in the
  uploaded blob (vs the live feed at `runner.rs:1750-1753`).
- [ ] Emit a timestamp on every uploaded log line so the toggle works.
- [ ] Keep live-feed and batch-upload timestamps consistent.
- [ ] Test the timestamp serialization format.

## 6. "Set up job" synthetic step parity (informative, grouped)

Observed: Velnor = 1 line; GitHub = full, grouped provisioning block. Reproduce
the structure with honest Velnor values, using `::group::` sections:

- [ ] `Current runner version: '…'` (keep).
- [ ] Grouped runner/image/OS info (the real Velnor job image + container OS).
- [ ] `GITHUB_TOKEN Permissions` group from the job message (`job_message.rs`).
- [ ] `Secret source`, `Prepare workflow directory`, `Prepare all required
  actions`, `Getting action download info`.
- [ ] One `Download action repository '<owner>/<action>@<ref>' (SHA:…)` line per
  resolved action, pulled from the plan (`action.rs`, `plan.rs`) — not hardcoded.
- [ ] `Complete job name: <display name>` as the final line.
- [ ] Omit hosted-only details rather than fabricate; match every step Velnor
  actually performs and consider adding Velnor-specific useful detail.
- [ ] Step uploads as `data-number=1`, populated + expandable.
- [ ] Test the "Set up job" content generation.

## 7. "Complete job" synthetic step parity

Observed: empty in Velnor; GitHub has cleanup content.

- [ ] Populate with the cleanup Velnor performs (stop job container, remove
  per-job network, clean work dir, recycle slot). Expandable.
- [ ] Test.

## 8. Step ordering, numbering, external IDs

- [ ] Confirm how `actions/runner` reserves numeric slots (GitHub: Post at
  #37–40, Complete #41; Velnor contiguous #21–25). Decide if numbering parity is
  required or cosmetic; document.
- [ ] If required, reserve pre/main/post slots so Post lands in the
  GitHub-equivalent range; Post order mirrors GitHub (reverse registration).
- [ ] Remove the leaking `-check` suffix on external IDs (`script_step.rs` /
  `executor.rs`); emit clean UUIDs while preserving pre/main/post correlation.
- [ ] Test ordering/numbering and external-id generation.

## 9. Annotations / warnings / errors

- [ ] Populate job-level annotations in `RunServiceCompleteJob` (empty today,
  `runner.rs:2803`) from collected step annotations.
- [ ] `::error::`/`::warning::`/`::notice::` render as UI annotations with
  file/line/col/title; counts correct.
- [ ] Test annotation propagation.

## 10. Step summaries (`GITHUB_STEP_SUMMARY`)

- [ ] Determine the job-summary upload path from `actions/runner`.
- [ ] Upload captured summaries (`script_step.rs:693,707`) so they render in the
  UI Summary tab.
- [ ] Test.

## 11. Masking / secrets (no regression)

- [ ] Masks applied to **every** newly-uploaded log (incl. previously-empty
  steps).
- [ ] Checkout token masking holds (`checkout.rs:1257-1282`).
- [ ] Test: secret never appears unmasked, including inside grouped/colored output.

## 12. Job outputs & conclusions

- [ ] Conclusion mapping matches GitHub (`runner.rs:3111-3119`).
- [ ] Outputs render; downstream `needs.*.outputs` resolve; environment URL
  surfaces when present.

## 13. Verification gates

- [ ] `cargo fmt --check`
- [ ] `cargo test -q`
- [ ] `cargo run -q -p velnor-tools -- check-runner-reference`
- [ ] Build images (`docker/job-ubuntu.Dockerfile`, root `Dockerfile`).
- [ ] Run a fixture smoke (see *Fixture proof* prompt); capture the Velnor lane
  render.
- [ ] Diff Velnor vs GitHub-hosted render; confirm §1–§12 resolved and the
  result is **not less informative** than GitHub anywhere.
- [ ] Update `../docs/comparison.md` with before/after and write
  evidence under `.velnor-live-evidence/`.

## 14. Definition of done

- [ ] Every executed step expandable; skipped match GitHub.
- [ ] Grouping, color, padding, timestamps present; user output passed through
  verbatim; adapter output equal-or-better than GitHub.
- [ ] Synthetic steps populated; ordering/IDs clean.
- [ ] Annotations, summaries, masking, outputs, conclusions correct.
- [ ] Comparison analysis done; all gates green; evidence recorded.
