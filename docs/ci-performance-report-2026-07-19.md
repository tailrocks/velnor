# CI Performance Campaign Report — 2026-07-19

This report closes the estate-standardization campaign against the clean host
state recorded in [host-baseline-2026-07-18.md](host-baseline-2026-07-18.md).
It does not turn missing evidence into a measurement: each incomplete
three-round workload has a named terminal blocker below.

## Method and safety boundary

- Every controlled dispatch cancelled earlier active runs, drained the exact
  repository daemon, deleted exact-prefix stale registrations, confirmed
  zero, started the daemon, confirmed fresh online registrations, and then
  monitored only the new run.
- Velnor packages were delivered only through the signed apt repository.
  The closing runner is v0.1.98; installed package, apt candidate, and job
  image label all matched before the final measurements.
- No workflow was weakened to hide a Velnor divergence. Unapproved action
  inputs or security surfaces failed under the strict manifest.

## Final accepted numbers

| Repository / workload | Velnor evidence | GitHub / paired evidence | Accepted result |
|---|---|---|---|
| `tailrocks/velnor` CI | `29677471132` attempt 2: 33 s | `29677531106`; both `29677683522` | Class B pass; zero dependency downloads/compiles/tool installs |
| `tailrocks/holla` CI | `29645393337`: 57 s | `29644855730`; both `29645094502` | Class B pass; zero dependency downloads/compiles/tool installs |
| `tailrocks/ruxel` CI | `29648690451`: 52 s | `29648738056`; both `29648808556` | Class B pass; zero dependency downloads/compiles/tool installs |
| `tailrocks/schemalane` CI | cold/warm/no-change `29656696148` / `29657441253` / `29657699348`; final jobs 19/33/21 s | both-lane evidence included in the rounds | Class D pass; final logs clean |
| `tailrocks/pg-bigdecimal` CI | rounds `29656785834` / `29656825488` / `29656852885`; no-change 21 s | standalone GitHub `29656700384`; both `29656727022` | Class D pass; final log clean |
| `tailrocks/tracing-request-level` CI | rounds `29657468067` / `29657519723` / `29657578558`; no-change 23 s | no-change GitHub 85 s; both `29657407165` | Class D pass; final log clean |
| `tailrocks/parallax-telemetry-playground` CI | Velnor `29659527093` | GitHub `29656744438`; warm/no-change both `29658825204` / `29659637769` | Class E pass; no-change has zero tool downloads/dependency compilation |
| `ChainArgos/java-monorepo` representative workflows | Velnor `29643642046` | GitHub `29643795326`; both `29644162613` | Workload gates pass; merge blocked by branch policy |
| `jackin-project/jackin` Docs | `29676495826` attempt 2 | GitHub `29676695763`; both `29677070745` | Docs performance audit pass; functional no-change path clean |
| `jackin-project/jackin` CI | `29677129499` | GitHub `29677193582`; both `29677241726` | Three-lane current-head caller proof passes |

Run identifiers are GitHub Actions run IDs in their named repository. Full
URLs and incident/fallback evidence are retained in
`plans/OPERATOR-REPORT.md`.

## Named blockers and excluded measurements

| Repository | Missing campaign evidence | Terminal blocker |
|---|---|---|
| `ChainArgos/blockchain-nodes` | accepted three-round package campaign | Velnor `29637999142` and GitHub `29638091607` fail the same three pre-existing package builds; Dockerfile/package-source fixes are outside plan 048 |
| `jackin-project/jackin` Preview | Velnor/paired Preview and complete three-round Class A campaign | Velnor run `29677351289` activates `actions/attest-build-provenance@v4`; manifest v3 explicitly rejects the unapproved Sigstore/GitHub attestation client surface |
| `tailrocks/parallax` browser workload | accepted Velnor rounds | GitHub `29664024963` passes; three Velnor attempts, including the documented 2-slot/8-CPU fallback, reproduce the same 3,683-pixel loading-skeleton divergence |
| `tailrocks/termrock` Rust workload | accepted lane/perf rounds | Velnor `29651695696` and GitHub `29651936604` fail identical pre-existing rustfmt drift outside plan 054 |
| `tailrocks/tablerock` | any program-branch campaign | repository `AGENTS.md` requires trunk-only delivery and the plan template would delete newer coverage; plan 057 could not safely create the mandated branch |

## Estate enforcement sweep

`velnor-tools audit-ci --estate config/estate-repositories.json --json` ran
on 2026-07-19. It reported 168 errors. Clean merged repositories such as
Schemalane, pg-bigdecimal, and tracing-request-level produced no errors;
findings on unmerged `main` worktrees map to the blocked plan rows above or
to java-monorepo's branch-protection block. The sweep is therefore not
misreported as zero-error. Its machine-readable output was summarized into
the terminal plan evidence and operator report.

## Required-check and cadence handoff

No organization/repository policy was guessed. For each merged repository,
require its lane-suffixed `required` aggregator on `main`; for an unmerged
program PR, use the exact required job shown by its green three-lane runs
after the PR merges. Decisions to require the GitHub comparison lane, enable
a remote GitHub compiler cache, or add a scheduled `both` matrix arm remain
operator-only because they change branch protection, cache trust/network
surface, or the canonical standard respectively. Exact contexts, delivery
prerequisites, and non-destructive verification are in
`docs/required-check-handoff.md`; the decision record remains in
`plans/OPERATOR-REPORT.md`.

## FINAL NUMBERS

- Fastest accepted library no-change result: 21 s (`pg-bigdecimal`).
- Velnor dogfood no-change: 33 s versus a 90 s Class B budget.
- Holla and Ruxel no-change: 57 s and 52 s, both under Class B budget.
- Tracing paired no-change: Velnor 23 s, GitHub 85 s.
- Every accepted Velnor no-change log above contains zero dependency download,
  dependency compilation, and tool-install markers.
- Five repositories remain explicitly excluded by named correctness or
  administrative blockers; no timing claim is made for their missing rounds.
