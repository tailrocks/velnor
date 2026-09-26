# V-BUN-001: preserve discovered package inputs

Status: deterministic checks and independent review passed; exact-push CI pending.
Baseline: `29279ab2` (same generator source as `0dcd61ad`).
Candidate: bounded S2 watch change, generator revision 59; commit pending.

## Hypothesis and structural cause

The watch primitive discards Bun scanner inputs, replacing them with root-level
source and configuration guesses. A nested package therefore loses its actual
source watches. Preserve scanner facts and add metadata relative to the package.

Alternatives: preserve scanner facts at the primitive boundary; introduce typed
input provenance throughout the scanner graph; or require consumers to duplicate
source globs. The first directly removes the destructive boundary. Typed input
provenance remains relevant to incomplete asset discovery. Consumer duplication
does not repair the generator default.

## Intervention and regression caught

Keep scanner watches for Bun units. Resolve package-manager and configuration
paths relative to each unit. Preserve existing root `src/**` and `scripts/**`
coverage: the initial draft removed these and would have lost CSS, assets, and
shell scripts. Parent regeneration caught that regression before commit.

This bounded change does not claim complete frontend dependency discovery.
The underlying Node/Bun scanner still enumerates JS/TS/JSON extensions, leaving
CSS/assets outside those existing root source families as a separate input-model
defect. That defect remains required follow-up; Docker parsing is separate too.

## Deterministic observations

- Isolated candidate: 1,784 library tests passed after regeneration; all-target
  Clippy, formatting, actionlint, and generator check passed.
- Independent reviewer `/root/jackin_inventory` checked the bounded patch and
  required a renderer-level regression beyond the helper tests. The added real
  scan/`WatchGraph::render` fixture passed with the candidate; restoring Bun to
  the destructive `derived` branch failed with exit 101 on missing `**/*.ts`.
  The focused watch suite passed five tests. Parent verified the staged source
  equals that isolated reviewed candidate and reran all-target Clippy.
- Velnor keeps every previous Bun watch and gains the five scanner extension
  globs. Commands and all other units remain unchanged. Preview path filtering
  receives the newly retained input globs through normal generation.
- Parallax temporary regeneration retains `ui/**/*.js/json/jsx/ts/tsx`, scoped
  locks, `ui/tsconfig.json`, and package build configurations. Its six Bun
  commands remain unchanged. Consumer migration is not committed until its
  remaining Docker/runtime dependencies are resolved.
- Local candidate compilation used an isolated dirty checkout. These tests
  prove behavior, not immutable binary provenance; clean rebuild and exact-push
  CI remain required. The separate source-identity defect is recorded in
  V-IDENTITY-001.
- Baseline evidence-only commit `29279ab2` subsequently failed generated-input
  policy: its new evidence paths changed the scan fingerprint without recording
  regeneration. The generated ownership update accompanying this candidate
  records the full staged file set. That failed run is retained separately as
  `observations/velnor-35487938941-*`; it is not a successful timing baseline.

No performance delta is claimed. This restores required relevance correctness
before measuring a correct baseline.
