# Plan 056: parallax-telemetry-playground — Class E (Class D + polyglot mise)

> **Executor instructions**: Follow step by step; verify each step; STOP
> conditions binding. Update the status row in the velnor repo's
> `plans/README.md` when done.
>
> **Drift check (run first)**: in
> `/Users/donbeave/Projects/tailrocks/parallax-project/parallax-telemetry-playground`,
> `git log --oneline -1` — planned against `c54de9d`. On excerpt mismatch, STOP.

## Status

- **Priority**: P3 (Phase 3 #12)
- **Effort**: S
- **Risk**: LOW (sample/playground repo)
- **Depends on**: plans/041-fixture-inline-matrix-and-snippets.md
- **Category**: dx
- **Planned at**: velnor repo commit `48b04ad`, target `c54de9d`, 2026-07-18

## Why this matters

The polyglot OTel/Sentry playground (Rust + Java) has **zero caching of any
kind**, no lanes, `ubuntu-latest` ×5, no timeouts, and behind-latest pins
(checkout v6.0.3, mise-action v4.1.0). Every CI run compiles cold. It is the
Class E template: Class D plus polyglot mise tools.

## Current state

Repo: `/Users/donbeave/Projects/tailrocks/parallax-project/parallax-telemetry-playground`,
`main`. Single `ci.yml`; concurrency present (1/1); root has `mise.toml`
(no `renovate.json`, no `rust-toolchain.toml` — verify which languages the
jobs actually build with `grep -n "run:" .github/workflows/ci.yml` before
assuming; the workflow has 5 jobs on ubuntu-latest and 4× mise-action,
3× upload-artifact v7.0.1).

Template: this plan reuses the **complete Class D `ci.yml` template from
plan 055** — copy the YAML block from
`plans/055-estate-library-trio-class-d.md` section "The Class D template"
(it is self-contained YAML; GitHub arm `ubuntu-26.04`, contract sccache env,
mold, mise, registry cache, nextest). Class E deltas below.

## Scope

**In scope**: `.github/workflows/ci.yml`, root `mise.toml` (add missing
tools), new `rust-toolchain.toml` (if Rust builds exist), `renovate.json`
(copy holla's), `.github/AGENTS.md`.
**Out of scope**: sample app sources, docker-compose/collector configs.

## Git workflow

Branch `velnor-estate-standard`; `ci:` commits, `git commit -s`; no push
without operator instruction.

## Steps

### Step 1: Inventory the real job surface

`python3 -c "import yaml; d=yaml.safe_load(open('.github/workflows/ci.yml')); [print(n, [s.get('name') or s.get('uses') or (s.get('run') or '')[:40] for s in j.get('steps',[])]) for n,j in d['jobs'].items()]"`
Record which jobs compile Rust, which build Java, which just lint/compose.

**Verify**: the inventory printed; keep it for the PR description.

### Step 2: Apply Class D template + Class E deltas

Replace lane-less jobs with the canonical matrix form (keep each job's
existing build/test commands as the steps after setup). Deltas per language:
Rust jobs get the full Class D setup chain; Java jobs get mise-managed JDK
(add to `mise.toml`, e.g. `java = "<current LTS>"`) + a Gradle/Maven cache
step keyed on the build lockfile (`actions/cache` on `~/.gradle/caches` or
`~/.m2/repository`, key includes the lane); non-compiling jobs get matrix +
timeout only. Bump all pins to latest majors (recipe: plan 052 step 1).

**Verify**: actionlint exit 0; `grep -n "ubuntu-latest" .github/workflows/ci.yml` → none.

### Step 3: Hygiene

`timeout-minutes: 20` per job (30 for Java builds); `persist-credentials:
false`; `renovate.json` + `.github/AGENTS.md` landed.

**Verify**: python timeout check exit 0; files exist.

## Test plan

actionlint. Operator: three-lane dispatch; second Velnor run must show warm
mise installs (no JDK/tool re-download in logs). §2.11 Class D/E budget
(≤ 60–90 s no-change rerun).

## Done criteria

- [ ] Lanes + matrix + caching on every job; zero ubuntu-latest; pins latest
- [ ] mise carries every toolchain the jobs use; timeouts everywhere
- [ ] Operator dispatches green or deferred; no out-of-scope changes

## STOP conditions

- A job drives docker-compose against the host Docker → it needs the trusted
  Velnor pool; if the playground pool is not trusted, STOP and report.
- Java build tool cache path ambiguous (neither Gradle nor Maven evident) →
  STOP with the step-1 inventory attached.

## Maintenance notes

- This is the Class E exemplar for future polyglot repos; keep its deltas
  minimal and documented in `.github/AGENTS.md`.
