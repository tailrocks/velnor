G-followup slice done. File-level `events` for `scheduled-checks` rows, per the revised Q1 design. No commit, no other files touched.

## Files changed

**`crates/velnor-workflow/src/primitives/check_profiles.rs`** (only production file touched):
- L12–27: module docs — `events` semantics, one-file-one-trigger-set, lanes note, why-not-a-CI-unit justification (code comment).
- L54–68: `schema()` gains `"events"`; `render()` passes the file name (not the primitive id) as the error label so usage errors name the file, then `select_profiles` → `select_events` → `render_checks_file`.
- L81–132: `select_profiles` signature unchanged (coverage caller in `mod.rs` untouched); reads `events` presence and threads `evented` into the cadence check.
- L135–174: `check_shared_cadence` extended — mixed scheduled/schedule-less is a "one trigger set" error; all-schedule-less without `events` is a no-trigger error naming file + profiles; mixed-cron message byte-identical.
- L176–224: `ACCEPTED_EVENTS` (`push`, `pull_request`, `workflow_dispatch`), `RENDERED_EVENT_ORDER`, and `select_events` — unknown/empty/repeated/mistyped `events` fail closed naming the file. `workflow_dispatch` validates but never double-renders (already unconditional). Full name validation lives on the render path (where `ctx.file` exists) rather than in `select_profiles`, because the coverage pre-check calls selection with the primitive id — validating there would label errors with the primitive instead of the file. Memo-letter deviation in service of memo intent; documented in the fn comment.
- L308–371: `render_checks_file` — `push:`/`pull_request:` keys, then shared `schedule:` (omitted when cron-less), then `workflow_dispatch:`; evented files get PR-only-cancel `cancel-in-progress: ${{ github.event_name == 'pull_request' }}` (docs-site/Renovate precedent), cron-only files keep `cancel-in-progress: true`. Old `render_scheduled_checks` kept as a `#[cfg(test)]` delegate so existing tests are byte-untouched.
- L876–1113: 9 new unit tests + 2 helpers (`unscheduled`, `render_with_events`).

**`crates/velnor-workflow/tests/scheduled_check_profiles.rs`**: L266–299 evented end-to-end test, L301–334 unknown-event fail-closed test (both mutate a temp copy of the existing fixture; no new fixture needed).

## Lanes decision: keep per-profile lanes, no file-level lane, no matrix
Each profile already declares `runner` (github/macos/velnor) and each job renders its own `runs-on` — the daily fixture file already mixes github + velnor jobs, so the file *is* the profile×lane matrix. Triggers change when a job runs, never where; a file-level lane would duplicate per-profile runners or need precedence rules. "One file, one trigger set" governs triggers only. Zero lane-code changed; evented jobs render identical `runs-on`/steps.

## Why not a CI unit
A scheduled check is a whole-repo compliance probe needing its own required status context plus main-branch runs independent of affected-unit selection. A CI unit renders only when the scan selects it and reports under unit lanes — folding a repo-wide gate into a unit would make compliance conditional on selection and lose the standalone required signal. (Also as module-doc comment, L20–27.)

## Acceptance evidence
1. Push/PR + shared cron: unit `evented_file_renders_push_pr_triggers_plus_shared_cron` + integration `evented_file_renders_push_pr_cron_and_pr_only_cancel` (also pins key order push → PR → schedule → dispatch).
2. Cron-less evented, no `schedule:` block: unit `cron_less_evented_file_renders_no_schedule_trigger`.
3. Schedule-less in cron file refused, names file: unit `schedule_less_profile_in_cron_file_is_refused`.
4. Uniformity: units `mixed_scheduled_and_schedule_less_profiles_are_refused`, `mixed_cadences_are_refused` (existing), `unknown_empty_repeated_and_mistyped_events_are_refused` + canonical-order/dispatch-dedupe tests.
5. PR-only-cancel: unit `evented_files_cancel_pull_requests_only` + integration assert on daily, `cancel-in-progress: true` on weekly.
6. Existing tests unmodified and green: 11/11 lib + 4/4 integration (existing fns untouched; only appends).

## Verification (tails)
- `cargo test -p velnor-workflow --lib primitives::check_profiles` → `test result: ok. 20 passed; 0 failed` (676 filtered out)
- `cargo test -p velnor-workflow --test scheduled_check_profiles` → `test result: ok. 6 passed; 0 failed`
- `cargo clippy -p velnor-workflow --all-targets` → 0 warnings
- `cargo fmt -p velnor-workflow -- --check` → clean (applied via `rustfmt` on my two files only)
- Manual generation eyeballed: evented daily shows `push:/pull_request:/schedule:/workflow_dispatch:` + PR-only-cancel; weekly unchanged classic shape.

## Known boundary (needs another slice, out of my ownership)
End-to-end cron-less is currently unreachable: `validate_check_profile_row` (`config/mod.rs`, not mine) rejects schedule-less rows before rendering — probed: `error: [[check_profile]] compat is missing 'schedule'...`, exit=1. This slice lands the `select_profiles` validation + renderer + unit contract; it activates as soon as config passes schedule-less specs through (`apply_check_profiles` already maps missing → `""`, which my code treats as schedule-less). Related: end-to-end, shared-selection errors (mixed schedules) surface from the coverage pre-check labeled with the primitive id, since that caller (`primitives/mod.rs:1350`, not mine) passes `row.primitive`; making those name the file is a one-line change there.