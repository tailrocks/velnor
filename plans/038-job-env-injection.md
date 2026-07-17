# Plan 038: Job-container env defaults — SCCACHE_CACHE_SIZE, SCCACHE_BASEDIRS, CARGO_INCREMENTAL=0

> **Executor instructions**: Follow step by step; run every verification; STOP
> conditions binding. Update the status row in `plans/README.md` when done.
>
> **Drift check (run first)**:
> `git diff --stat 48b04ad..HEAD -- crates/velnor-runner/src/runtime_env.rs crates/velnor-runner/src/container.rs`
> Re-locate by symbol; mismatch → STOP.

## Status

- **Priority**: P0 (V0.9)
- **Effort**: S
- **Risk**: LOW-MED (env precedence must stay truthful)
- **Depends on**: none (independent; complements 034)
- **Category**: perf / stability
- **Planned at**: commit `48b04ad`, 2026-07-18

## Why this matters

Cache hygiene requires the runner to enforce three defaults on every job
container: `SCCACHE_CACHE_SIZE` (bounded store — without it sccache's local
store grows unbounded on the shared host), `SCCACHE_BASEDIRS` (path-normalized
keys across `/__w` and the job-home mounts so identical code hits across
repos/slots), and `CARGO_INCREMENTAL=0` (incremental session dirs are the
target-bomb class). None exist in the runner today; estate workflows set
`CARGO_INCREMENTAL` themselves, but the runner must not depend on workflow
discipline (13 repos × drift).

## Current state (verified against `48b04ad`)

- `crates/velnor-runner/src/runtime_env.rs:4-19` (verified) —
  `job_runtime_env()` builds the base defaults vec (`CI`, `GITHUB_ACTIONS`,
  `HOME=/github/home`, `GITHUB_WORKSPACE=/__w`, `RUNNER_TEMP=/__t`, ...).
  Workflow env is appended AFTER the defaults (`runtime_env.rs:180-185`),
  skipping protected names (`is_protected_default_env`, `:295-300` —
  GITHUB_/RUNNER_/ACTIONS_/AGENT_TOOLSDIRECTORY). Later value wins when
  collected into a BTreeMap (`executor.rs:4913`) — so defaults placed in the
  base vec are workflow-overridable; names added to the protected list are
  not.
- `crates/velnor-runner/src/container.rs:128-140` (verified) — container `-e`
  flags: `HOME`, `RUSTUP_HOME=/root/.rustup`, `CARGO_HOME=/github/home/.cargo`,
  `SCCACHE_DIR=/var/cache/sccache`, `RUNNER_TEMP`, `RUNNER_TOOL_CACHE`.
- `grep -rn "SCCACHE_CACHE_SIZE\|SCCACHE_BASEDIRS" crates/` → no matches
  (net-new).
- Tests: `runtime_env.rs:472+` (`builds_github_runtime_env_from_job_message`,
  which already exercises CARGO_INCREMENTAL/SCCACHE_DIR override behavior at
  `:505-513`, `:567-569`); container arg tests `container.rs:1043+`.

Precedence decisions (binding):
- `CARGO_INCREMENTAL=0` — overridable default (base vec): a workflow that
  explicitly sets `CARGO_INCREMENTAL=1` is making a declared choice; the
  runner default only covers the silent case.
- `SCCACHE_CACHE_SIZE=20G` — overridable default (contract's manifest owns
  what values workflows MAY set; enforcement of allowed values is 033/034's
  validation, not env protection).
- `SCCACHE_BASEDIRS` — runner-computed truth (`/__w` + `/github/home`);
  workflow override is meaningless and dangerous → add to the PROTECTED list.
  CAVEAT: verify sccache v0.16.0 actually supports `SCCACHE_BASEDIRS`
  (upstream docs/CHANGELOG) — if the variable name or semantics differ
  (e.g. only `SCCACHE_C_BASEDIR` class options exist), STOP and report the
  real mechanism before wiring anything.

## Commands you will need

| Purpose | Command | Expected |
|---------|---------|----------|
| Gates | fmt / clippy / `cargo nextest run --workspace --locked` | exit 0 |
| Focused | `cargo nextest run -p velnor-runner runtime_env` | pass |

## Scope

**In scope**: `runtime_env.rs`, `container.rs` (only if the daemon-tunable
env knobs need plumbing), `cli.rs` (env-backed overrides
`VELNOR_SCCACHE_CACHE_SIZE`, pattern `cli.rs:210-215`), tests.
**Out of scope**: the sccache adapter script (034), estate YAML, manifest
rules (033).

## Git workflow

Branch `velnor-estate-standard`; `feat(runner):` commit, `git commit -s`; no
push without operator instruction.

## Steps

### Step 1: Defaults in `job_runtime_env`

Add to the base vec (`runtime_env.rs:5-19`): `CARGO_INCREMENTAL=0`,
`SCCACHE_CACHE_SIZE=<daemon value, default "20G">` (plumb the value in as a
parameter or via the same config path `job_runtime_env` already receives —
inspect its callers at `runner.rs:3582` for the cleanest thread; a
`OnceLock`-read env `VELNOR_SCCACHE_CACHE_SIZE` is acceptable if threading a
param ripples > 3 call sites).

**Verify**: extend `builds_github_runtime_env_from_job_message` — defaults
present; a job message that sets `CARGO_INCREMENTAL=1` in workflow env wins.

### Step 2: Protected `SCCACHE_BASEDIRS`

After the upstream-support check (Current state caveat): compute
`SCCACHE_BASEDIRS=/__w:/github/home` (real separator per upstream docs), add
to the base vec AND to `is_protected_default_env` (`runtime_env.rs:295-300`)
so workflow values are skipped.

**Verify**: new test `sccache_basedirs_not_overridable_by_workflow_env`.

### Step 3: Docs

`docs/runner-usage.md`: add the three defaults + the daemon override knob to
the env table (locate the existing env-var table; keep format).

**Verify**: grep the doc for the three names.

## Test plan

3 focused tests (defaults present, override semantics both directions,
protected basedirs). Full gates green.

## Done criteria

- [ ] Gates exit 0; tests pass
- [ ] `grep -rn "SCCACHE_CACHE_SIZE" crates/velnor-runner/src/runtime_env.rs` hits
- [ ] Docs updated; no out-of-scope changes

## STOP conditions

- sccache v0.16.0 lacks `SCCACHE_BASEDIRS` (see caveat) → implement steps
  1 + 3 only, STOP on step 2 with the upstream findings.
- Threading the daemon knob ripples > 3 call sites and the OnceLock escape
  feels wrong → STOP, propose the seam.

## Maintenance notes

- 034's adapter script must not fight these defaults (it sets
  SCCACHE_CACHE_SIZE only if unset — coordinate at review if both in flight).
- If kache needs equivalent bounding env, add it in 034, not here.
