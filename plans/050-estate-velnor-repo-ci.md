# Plan 050: velnor repo CI — dogfood the estate standard

> **Executor instructions**: Follow step by step; run every verification; STOP
> conditions binding. Update the status row in `plans/README.md` when done.
>
> **Drift check (run first)**: `git log --oneline -1` in
> `/Users/donbeave/Projects/tailrocks/velnor-project/velnor` — planned against
> commit `48b04ad` (branch `docs/velnor-projects-setup-standard`; the CI files
> referenced live on `main` too — verify `.github/workflows/` matches the
> excerpts). On mismatch, STOP.

## Status

- **Priority**: P1 (Phase 2 #4 — the runner must run its own standard)
- **Effort**: M
- **Risk**: MED (this repo releases the runner .deb; CI is the safety net)
- **Depends on**: plans/041-fixture-inline-matrix-and-snippets.md
- **Category**: dx / perf
- **Planned at**: commit `48b04ad`, 2026-07-18

## Why this matters

Velnor's own repo runs `ubuntu-latest` ×10, has no lane inputs at all, no
`timeout-minutes`, and concurrency on only 2 of 4 workflows. The runner
project not following its own published standard (`VELNOR_PROJECTS_SETUP.md`)
is the first credibility failure an estate maintainer will notice. Dogfooding
also gives the fleet a continuous self-test: every Velnor PR exercises the
runner on its own CI.

## Current state

Repo: `/Users/donbeave/Projects/tailrocks/velnor-project/velnor`.
Workflows: `ci.yml`, `release-deb.yml`, `release.yml`, `renovate.yml`.
Verified 2026-07-18: 10× `runs-on: ubuntu-latest`, 1× `ubuntu-24.04`,
1× `${{ matrix.os }}`; pins current (checkout v7, cache v6, mise-action
v4.2.0, sccache-action v0.0.10, setup-mold v1); mold + sccache + registry
cache already present in `ci.yml`; `CARGO_INCREMENTAL: "0"` set; zero
`timeout-minutes`; no `lanes` input anywhere; `renovate.yml` pinned
`v46.1.18` (older than estate's `v46.1.19` — bump while here).

Verification commands for this repo (from `mise.toml`):
`mise run fmt` / `mise run lint` / `mise run test` / `mise run actionlint`
(or the raw cargo equivalents listed in `plans/README.md`).

Canonical blocks — identical to plan 047 ("Current state" section): the
`lanes` dispatch input (default `velnor`), the one-line inline matrix with
`{"lane","runner","writer"}` records (`ubuntu-26.04` GitHub arm,
`["self-hosted","velnor-target-mvp"]` Velnor arm), and the contract env
(`SCCACHE_GHA_ENABLED: "false"`, `SCCACHE_CACHE_SIZE: 20G`). Copy them from
`VELNOR_PROJECTS_SETUP.md` §2.1/§2.4/§9 in this repo — they are in-tree.

## Scope

**In scope**: `.github/workflows/*.yml`; a new `.github/AGENTS.md` (three
lanes + ubuntu-26.04 + "GitHub lane kept for fleet-break recovery").

**Out of scope**: all crates, docs, mise.toml, Dockerfile, deb packaging.
`release.yml`/`release-deb.yml` STAY runnable on the GitHub lane even after
Velnor default (fleet-break recovery path — the release must be buildable
when the fleet is down).

## Git workflow

Branch `velnor-estate-standard`; conventional `ci:` commits,
`git commit -s`; no push without operator instruction.

## Steps

### Step 1: `ci.yml` — lanes + inline matrix

Add the `lanes` dispatch input; give every job the canonical inline matrix;
`runs-on: ${{ matrix.config.runner }}`; job names gain
`(${{ matrix.config.lane }})`. Add `SCCACHE_GHA_ENABLED: "false"` +
`SCCACHE_CACHE_SIZE: 20G` to the workflow env (confirm no step re-enables GHA).

**Verify**: `mise run actionlint` → exit 0;
`grep -n "ubuntu-latest" .github/workflows/ci.yml` → none.

### Step 2: `release.yml` / `release-deb.yml` — lanes with GitHub-writer default kept dispatchable

Add `lanes` input (default `velnor`); inline matrix; writer-gate the publish
steps (deb upload, apt repo push) with `if: matrix.config.writer`. The deb
build itself must succeed on both lanes.

**Verify**: actionlint exit 0; `grep -n "matrix.config.writer" .github/workflows/release-deb.yml` shows gating on every publish step.

### Step 3: `renovate.yml` — single-writer, pinned Ubuntu, bump action

`ubuntu-latest` → `ubuntu-26.04` (or inline matrix single-writer if the
operator wants Renovate on Velnor — leave on GitHub lane by default, comment
why: Renovate needs no warm caches and must run when the fleet is down).
Bump `renovatebot/github-action` to the latest release SHA at PR time.

**Verify**: actionlint; `grep -n "ubuntu-latest" .github/workflows/` → none.

### Step 4: Hygiene

`timeout-minutes` on every job (ci jobs 30; release 45; renovate 30);
`concurrency` on `release.yml`/`renovate.yml` (no cancel on release);
`persist-credentials: false` on non-pushing checkouts; keep `fetch-depth`
default everywhere (no fetch-depth: 0 exists today — keep it that way).

**Verify**: python timeout check (plan 047 step 5 snippet) exit 0 per file.

### Step 5: `.github/AGENTS.md`

Write the three-lane + ubuntu-26.04 + recovery-lane policy (≤30 lines).

**Verify**: file exists; states Velnor default, GitHub = pinned 26.04
comparison + fleet-break recovery, both = parity.

## Test plan

`mise run check` still green (workflow changes must not break the Rust gates).
Operator: dispatch ci.yml on all three lanes; `both` produces identical job
sets; record §2.11 timing table in the PR. Velnor-lane no-change rerun budget:
Class B ≤ 90 s.

## Done criteria

- [ ] actionlint + `mise run check` exit 0
- [ ] Zero `ubuntu-latest`; every job `timeout-minutes`; 4/4 workflows concurrency
- [ ] Three-lane dispatch green (operator) or explicitly deferred
- [ ] Release workflows remain green on `lanes=github` (recovery drill)
- [ ] No out-of-scope modifications

## STOP conditions

- Velnor lane fails on capability error → STOP; runner fix, never workflow
  workaround (this repo especially: the temptation to hack is highest here).
- The self-hosted label `velnor-target-mvp` is not registered for this repo →
  STOP; fleet onboarding (plan 039) must come first.

## Maintenance notes

- Once plan 046 lands, add this repo to the `audit-ci` estate list.
- The GitHub recovery lane must be re-drilled after every major release
  workflow change — reviewers should ask for the drill result.
