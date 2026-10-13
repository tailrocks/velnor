# Goal: GitHub UI Experience Parity for Velnor Runner Output

> **Mission:** see [`../docs/mission.md`](../docs/mission.md). Velnor is the Rust-first,
> self-hosted runner that is faster, cheaper, and nicer than GitHub-hosted.
> This goal owns the "nicer, more informative output" pillar — and should make
> Velnor's speed/cost/Rust advantages *visible* in the log (cache hits, sccache
> stats, parallelism, time saved), not just match GitHub.

## Objective

Make a Velnor-executed job's GitHub Actions UI **as good as — or better than —
the GitHub-hosted runner**, and never less informative. The target is not a
byte-for-byte clone (that is neither possible nor desirable), but an experience
that is **as close as we can achieve**: every step expandable, logs grouped and
colored, padded and readable, with faithful synthetic steps, timestamps,
annotations, and conclusions. Where Velnor can make the output *clearer, more
modern, and nicer* than GitHub, it should.

## Why

GitHub renders the Checks UI from what the runner reports through the Results
Service. GitHub's output is highly informative and convenient: collapsible
groups, ANSI colors, consistent padding, per-line timestamps — all of which make
debugging easy. Today a Velnor job *succeeds* but the UI is poorer than the
hosted lane: several steps are **not expandable** (no log uploaded), the "Set up
job" step is nearly empty, there are no per-line timestamps, no log grouping, no
color. Phase 0 completion requires GitHub-UI evidence that stands next to the
comparison lane without looking worse — see [`docs/roadmap.md`](../docs/roadmap.md)
"Verification Strategy".

## Guiding principle: we control our output, so make it excellent

Velnor does **not** run marketplace JavaScript or shim everything through bash.
Native Rust adapters do the work, which means **Velnor controls the log stream**
and can make it more informative, modern, and nicer than GitHub's. Use that:

- **Native adapter output** (checkout, cache, sccache, mise, docker, …) — Velnor
  authors it. Make it grouped, colored, padded, and clear; at least as
  informative as GitHub's equivalent step, ideally better.
- **User-command output** (a `run:` step's stdout/stderr) — Velnor only invokes
  the command and forwards what it produces, **verbatim**. Preserve the user's
  bytes (including their ANSI). Do not rewrite user output; only frame it
  (group header, timestamps) the way GitHub does.

## Ground truth

Two captured renders of the **same fixture job** (`compat (app-a, …)`,
run `26872768024`) are the reference:

- GitHub-hosted lane — job `79252067791`
- Velnor lane — job `79252067855`

The detailed, observed divergences and the comparison method are recorded in
[`../.velnor-compare/comparison.md`](../.velnor-compare/comparison.md). When a
behavior is ambiguous, consult the official runner:
<https://github.com/actions/runner> (`src/Runner.Worker`, `src/Runner.Common`,
results-service / live-logs).

## In scope

- Per-step log upload so **every** executed step is expandable.
- Per-line timestamps (so the "Show timestamps" toggle works like GitHub).
- Log grouping (`::group::` / `::endgroup::`) producing collapsible sections.
- ANSI color parity/improvement for Velnor-authored adapter output.
- Faithful, informative "Set up job" and "Complete job" synthetic steps.
- Step ordering, numbering, and clean external IDs.
- Annotations / warnings / errors / step summaries surfaced to the UI.
- Job-level outputs and conclusions.
- A documented analysis of *what to improve and how*, plus collected evidence.

## Out of scope

- End-to-end fixture lane greenness (*Fixture proof completion* prompt).
- New native action adapters (*Target workflow coverage* prompt).
- Rewriting user-command stdout/stderr (must pass through verbatim).
- Changing the fixture to mask a gap (forbidden).

## Definition of done

- A Velnor fixture job's Checks UI is **at least as informative and convenient**
  as the GitHub-hosted lane: every step expandable, logs grouped + colored +
  timestamped + padded, synthetic steps populated, ordering/IDs clean,
  annotations/summaries/conclusions correct.
- A written comparison + improvement analysis exists and its actions are done.
- `cargo fmt --check` and `cargo test -q` pass; new behavior is tested.
- Evidence captured showing the matched/improved UI.

## Work through

➡ **[github-ui-parity.checklist.md](github-ui-parity.checklist.md)**
