# Velnor vs GitHub-hosted Runner — UI Output Comparison

Evidence and analysis for the **GitHub UI Experience Parity** goal
(`../prompts/github-ui-parity.md`). Mission context: `../docs/mission.md` — the
output must be **equal-or-better, never less informative**, and Velnor controls
its own log stream so it can be nicer than GitHub.

## Reference run

Same fixture workflow, two lanes of one run:

| Lane | Job ID | Step name |
|------|--------|-----------|
| GitHub-hosted | `79252067791` | `compat (app-a, github, "ubuntu-latest")` |
| Velnor | `79252067855` | `compat (app-a, velnor, ["self-hosted","velnor-target-mvp"])` |

Run: `26872768024` · repo: `donbeave/velnor-actions-fixture`.

Raw render captured: `.velnor-compare/velnor-job.html` (Velnor lane). Re-capture
both lanes fresh when iterating.

## Preferred extraction method: the GitHub API (not HTML scraping)

HTML scraping is lossy. Use the API for accurate, structured data and diff the
two jobs field-by-field. Examples:

```sh
# Job + steps (names, numbers, conclusions, started/completed timestamps)
gh api repos/donbeave/velnor-actions-fixture/actions/jobs/79252067791
gh api repos/donbeave/velnor-actions-fixture/actions/jobs/79252067855

# Full run + all jobs
gh api repos/donbeave/velnor-actions-fixture/actions/runs/26872768024
gh api repos/donbeave/velnor-actions-fixture/actions/runs/26872768024/jobs

# Raw logs (zip of per-step logs) — inspect grouping, color codes, timestamps
gh api repos/donbeave/velnor-actions-fixture/actions/jobs/79252067791/logs > github.zip
gh api repos/donbeave/velnor-actions-fixture/actions/jobs/79252067855/logs > velnor.zip
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

## Status

- [ ] Both lanes re-captured via the GitHub API.
- [ ] Field-by-field step + log diff recorded here.
- [ ] Improvement plan items implemented (track in the UI parity checklist).
- [ ] After/again render shows Velnor **not less informative** than GitHub.
