# Velnor vs GitHub-hosted Runner — UI Output Comparison

Evidence and analysis for the **GitHub UI Experience Parity** goal
(`../prompts/github-ui-parity.md`). Mission context: `mission.md` — the
output must be **equal-or-better, never less informative**, and Velnor controls
its own log stream so it can be nicer than GitHub.

## Reference run

Same fixture workflow, two lanes of one run:

| Lane | Job ID | Step name |
|------|--------|-----------|
| GitHub-hosted | `79252067791` | `compat (app-a, github, "ubuntu-latest")` |
| Velnor | `79252067855` | `compat (app-a, velnor, ["self-hosted","velnor-target-mvp"])` |

Run: `26872768024` · repo: `tailrocks/velnor-actions-fixture`.

Raw render captured: `../.velnor-compare/velnor-job.html` (Velnor lane). Re-capture
both lanes fresh when iterating.

## Preferred extraction method: the GitHub API (not HTML scraping)

HTML scraping is lossy. Use the API for accurate, structured data and diff the
two jobs field-by-field. Examples:

```sh
# Job + steps (names, numbers, conclusions, started/completed timestamps)
gh api repos/tailrocks/velnor-actions-fixture/actions/jobs/79252067791
gh api repos/tailrocks/velnor-actions-fixture/actions/jobs/79252067855

# Full run + all jobs
gh api repos/tailrocks/velnor-actions-fixture/actions/runs/26872768024
gh api repos/tailrocks/velnor-actions-fixture/actions/runs/26872768024/jobs

# Raw logs (zip of per-step logs) — inspect grouping, color codes, timestamps
gh api repos/tailrocks/velnor-actions-fixture/actions/jobs/79252067791/logs > github.zip
gh api repos/tailrocks/velnor-actions-fixture/actions/jobs/79252067855/logs > velnor.zip
```

Diff the two jobs' step arrays and the two log archives. Record per-step:
expandable?, has log?, timestamps?, `::group::` sections?, ANSI color?, padding,
content richness. Prefer adding a Rust `velnor-tools` subcommand to fetch + diff
these (mission: Rust-first automation) so the comparison is repeatable.

## Observed divergences (from the captured renders)

| # | Area | GitHub-hosted | Velnor | Action |
|---|------|---------------|--------|--------|
| 1 | Expandability | Every step has a log → expandable | `checkout` (#2), `sccache` (#7), some `set -euo pipefail` (#10,#18), `write-result.py` (#19), Post (#23,#24), `Complete job` (#25) have empty `data-log-url` → **not expandable** | Upload a log blob + metadata for every executed step |
| 2 | Per-line timestamps | RFC1123 GMT per line (`CheckStep-line-timestamp`) | absent → "Show timestamps" empty | Emit timestamp on every uploaded line |
| 3 | `::group::` collapsible sections | "Set up job" + steps use `js-checks-log-group` | none | Emit grouping; pass user groups through; group Velnor adapter output |
| 4 | ANSI color | colored (`ansifg-m`, `ansifg-c`, …) | mostly plain | Preserve user ANSI; add tasteful color to adapter output |
| 5 | "Set up job" richness | ~40 lines: runner version, image provisioner, OS, runner image, `GITHUB_TOKEN Permissions`, prepare dir, prepare/download actions, complete job name | **1 line** (`Velnor Runner/0.1.0 (protocol: 2.334.0)`) | Build a faithful, grouped, informative Set up job |
| 6 | "Complete job" | cleanup content | empty | Populate with Velnor cleanup actions |
| 7 | Step numbering | reserves slots (Post at #37–40, Complete #41) | contiguous (Post #21–24, Complete #25) | Decide parity vs cosmetic; match if required |
| 8 | External step IDs | clean UUIDs | one ends `-check` (#18) — leaks internal scheme | Emit clean UUIDs |
| 9 | Padding/readability | consistent indentation, scannable | varies | Audit + standardize adapter output |

> Note: step #16 differs by lane content (`Check MSRV` vs `cargo check … --locked`).
> Verify whether this is an expected `if:`/matrix difference or a real divergence
> when re-running; do not assume.

## Improvement plan (equal-or-better)

Velnor authors its own output for native adapters, so beyond *matching* GitHub we
should *improve*:

- A shared adapter-output style (group headers, info/warn/error coloring,
  key→value lines) so every native adapter (checkout, cache, sccache, mise,
  docker, …) looks consistent and modern.
- Group noisy output (apt installs, dependency resolution) behind `::group::`.
- Surface Rust-relevant, performance-relevant facts GitHub does not: cache
  hit/miss + bytes saved, sccache stats, parallelism used, time saved vs a cold
  run — making Velnor's speed/cost advantage visible in the log.
- Keep **user-command** stdout/stderr verbatim (including their ANSI); only frame
  it (group header + timestamps) the way GitHub does.

## Implementation status

### Implemented (Phase 0, this pass)

| # | Area | Before | After |
|---|------|--------|-------|
| 1 | Expandability | Many steps not expandable (no log blob uploaded) | Every non-skipped step gets a log blob → expandable |
| 2 | Per-line timestamps | Absent in uploaded blobs | RFC3339 prefix on every uploaded line; "Show timestamps" toggle works |
| 3 | `::group::` collapsible sections | None in synthetic steps | "Set up job" and "Complete job" use `##[group]`/`##[endgroup]` sections; user `::group::` passes through |
| 4 | ANSI color | Mostly plain | Cache hit/miss colored green/yellow; metadata header bold-cyan; cache path cyan |
| 5 | "Set up job" content | 1 line (runner version only) | Runner version + OS group + image group + permissions + action list + job name |
| 6 | "Complete job" content | Empty | Post-job cleanup group (container stop, network remove, dir clean, slot recycle) |
| 7 | Annotations | Empty in job completion | Step-level annotations aggregated into `RunServiceCompleteJob.annotations` |
| 8 | Step summaries | Only in step log (appended as text) | Separate `upload_step_summary` call to Results Service `CreateStepSummaryMetadata` |
| 9 | Checkout step log | Empty lines → not expandable | Syncing repo URL, ref, fetch depth, destination, result |
| 10 | Cache step content | Plain text only | Colored hit/miss with timing ms |

### Remaining (post-Phase-0 or cosmetic)

| # | Area | Status |
|---|------|--------|
| 1 | Per-`job-log` artifact naming (`job-log-<job>`) | master-plan P4.4 |
| 2 | Raw-log download 404s on Velnor V2 jobs (no v1 archive) | Documented; `job-log` artifact is the workaround (master-plan P4.3) |

## The repeatable comparison: `velnor-tools lane-compare` (P4.1)

```sh
cargo run -q -p velnor-tools -- lane-compare                  # latest compat.yml run
cargo run -q -p velnor-tools -- lane-compare --run-id <id>    # specific run
cargo run -q -p velnor-tools -- lane-compare --repo owner/x --workflow ci.yml
```

Pairs the `github`/`velnor` lane jobs of one run by job name, diffs every
step by number (display name, conclusion, duration, **expandability** from
the job-page `<check-step data-log-url>` attributes — exactly what the UI
renders), and checks lane log content (timestamps / `##[group]` / ANSI) from
the GitHub per-job log download vs the Velnor `job-log` artifact. Exit code
enforces the gate: **zero rows where GitHub shows information Velnor lacks**
(`--strict false` to report without failing). Report + raw evidence land in
`.velnor-compare/lane-compare-run-<id>/`.

### lane-compare baseline findings (run 27319103370, runner 0.1.17)

The first tool run found 12 WORSE rows — all fixed in the same pass:

| Finding | Root cause | Fix |
|---------|-----------|-----|
| Main `actions/checkout` step missing from the step list (#2) | Eager-checkout step events carried a non-GUID external id (`checkout1`); the Results Service drops such records | Route through `github_backend_step_id` (UUID) for start+log events |
| `${{ inputs.packages }}` raw in a step name | Display names never expression-evaluated | Resolve `${{ }}` at emit time with job contexts (upstream `TryUpdateDisplayName`) |
| `msrv` / `command-files` ids shown as step names | `step.name` (= YAML id) used as display-name fallback; GitHub never does | Match `ActionRunner.GenerateDisplayName`: DisplayName else `Run <first script line>` |
| Composite shown as inner step (`Run set -euo pipefail` instead of `Run ./.github/actions/check-fixture-output`) | Each embedded step registered its own timeline step | One step per composite (CompositeFrame); embedded output appends as `##[group]<inner>` sections — upstream CompositeActionHandler semantics |
| `job-log` artifact lines untimestamped | Artifact built without blob prefixes | Every artifact line now carries the 7-digit blob timestamp |
| Whole step log wrapped in one collapsed group + trailing `Finishing:` line | Velnor-invented format | GitHub format: group wraps ONLY the header (command, `with:`/`env:`); output visible below; `::group::`→`##[group]` converted **in place**; script body bold-cyan; summaries upload separately, never inline |

Expandability was already at parity in the baseline (every executed step on
both lanes expandable; skipped steps non-expandable on both). External ids
are clean UUIDs (`data-external-id` verified via HTML).

## Status (2026-06-11)

- [x] Improvement plan items implemented (§1–§10 above).
- [x] Live per-line streaming verified in production (0.1.5+ on Sentry):
      step lines appear in the UI while the step runs; per-step feed line
      numbering fixed in 0.1.6 (was a job-global counter — protocol mismatch
      vs actions/runner).
- [x] Downloadable-archive workaround shipped: the full masked job log is
      uploaded as a `job-log` artifact (per-job naming still pending,
      master-plan P4); artifact lines carry blob timestamps as of 0.1.18.
- [x] API-driven lane comparison shipped as `velnor-tools lane-compare`
      (master-plan P4.1) — repeatable, CI-runnable, equal-or-better gate.
- [ ] Post-0.1.18 fixture re-run: lane-compare must report **PASS** (0 worse
      rows); capture before/after evidence under `.velnor-live-evidence/`.
