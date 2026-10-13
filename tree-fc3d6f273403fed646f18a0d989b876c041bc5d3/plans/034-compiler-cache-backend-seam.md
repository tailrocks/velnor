# Plan 034: Compiler-cache backend seam — sccache v0.16.0 baked in, native kache, `off`

> **Executor instructions**: Follow step by step; run every verification; STOP
> conditions binding. Update the status row in `plans/README.md` when done.
>
> **Drift check (run first)**:
> `git diff --stat 48b04ad..HEAD -- crates/velnor-runner/src/executor.rs crates/velnor-runner/src/container.rs docker/`
> Re-locate excerpts by symbol on drift; semantic mismatch → STOP.

## Status

- **Priority**: P0/P1 boundary (contract gates 4–5; V2.2/V2.3 pulled forward
  because the sccache adapter currently violates the contract)
- **Effort**: L
- **Risk**: MED (compile-path behavior of every Rust job)
- **Depends on**: plans/033-strict-capability-manifest.md (input validation +
  ref plumbing; the kache manifest entry lands here)
- **Category**: perf / correctness
- **Planned at**: commit `48b04ad`, 2026-07-18

## Why this matters

`docs/strict-capability-contract.md` (READ FIRST — requirement source)
mandates: exactly one of `sccache | kache | off` per job, local-only stores,
pinned binaries **baked into the job image — never downloaded during a job**,
adapter-owned setup/post reporting, and exact pinned action inputs. Today's
adapter: downloads sccache **v0.15.0** at job time if missing (contract pins
v0.16.0), ignores all inputs, and has no kache or `off` path. The equal-budget
sccache-vs-kache comparison the operator approved cannot run until this seam
exists.

## Current state (verified against `48b04ad`)

- `crates/velnor-runner/src/executor.rs:1945-1954` — `native_sccache` ignores
  inputs (`_action`) and runs `sccache_setup_script()`.
- `executor.rs:2982-3022` — the script: `if ! command -v sccache; then
  ver="v0.15.0"; curl -fsSL https://github.com/mozilla/sccache/releases/...`
  (job-time download, wrong version), then writes to `$GITHUB_ENV`:
  `SCCACHE_DIR=/var/cache/sccache`, `SCCACHE_GHA_ENABLED=false`, exports the
  same, `sccache --start-server`. NOTE: the comment block at
  `executor.rs:2988-2989` claims the GHA backend is used — stale, the code
  forces local; delete the comment with the script.
- `crates/velnor-runner/src/container.rs:82` — sccache host store bind-mount →
  `/var/cache/sccache`; `container.rs:136` — container env
  `SCCACHE_DIR=/var/cache/sccache` (repeated for sidecar variants at
  `:406`/`:496`).
- Store root: `container.rs:762` `sccache_host` → `<work-root>/_velnor_sccache`
  (canonical `/var/cache/velnor/v1/<trust>/compiler/sccache` layout is plan
  035's job — this plan keys the mount through a helper so 035 relocates one
  function).
- Job image: `docker/` holds the Ubuntu job image build (confirm file name
  with `ls docker/`; the image is `velnor/job-ubuntu` per
  `VELNOR_PROJECTS_SETUP.md` §1).
- Contract exact configs (inline for convenience; the contract file is
  authoritative): sccache action allows only `version: v0.16.0`,
  `disable_annotations: "false"`; kache action
  `kunobi-ninja/kache-action@49398d37113c616fdb61be434cb497e3c2c8f3e6 # v1`
  allows only `version: v0.10.0`, `github-cache: "false"`,
  `cache-executables: "false"`, `pr-comment: "false"`, `max-size: 20GiB`;
  everything else rejected. Both wrappers in one job = preflight error.
  `SCCACHE_CACHE_SIZE` default 20G (env injection is plan 038's; this plan
  enforces the store-size limit for the local server via the same value).

## Commands you will need

| Purpose | Command | Expected |
|---------|---------|----------|
| Gates | `cargo fmt --all --check` / `cargo clippy --workspace --all-targets --locked -- -D warnings` / `cargo nextest run --workspace --locked` | exit 0 |
| Image build (operator/docker) | `docker build -f docker/<job-image-file> .` | image builds; `sccache --version` = 0.16.0, `kache --version` = 0.10.0 |

## Scope

**In scope**: `crates/velnor-runner/src/executor.rs` (sccache/kache/off
adapters), `container.rs` (env/mount seams), `manifest.rs` (kache entry,
dual-wrapper rule), `docker/` job-image (bake both binaries),
`docs/strict-capability-contract.md` (status notes).
**Out of scope**: canonical `/var/cache/velnor` path migration (plan 035);
`SCCACHE_BASEDIRS`/`CARGO_INCREMENTAL` env injection (plan 038); the
comparison experiment itself (operator-run, storage doc §Required comparison
experiment); estate YAML.

## Git workflow

Branch `velnor-estate-standard`; `feat(runner):` commits, `git commit -s`;
no push without operator instruction.

## Steps

### Step 1: Backend seam type

In `executor.rs` (or a new `compiler_cache.rs` module if executor.rs growth
is unwieldy — prefer the new module), define:

```rust
pub enum CompilerCacheBackend { Sccache, Kache, Off }
```

Selection per job at preflight: derived from which setup action the job
contains (`mozilla-actions/sccache-action` → Sccache;
`kunobi-ninja/kache-action` → Kache; neither → Off). BOTH present →
`CapabilityViolation` (add the rule to `manifest::validate_job` from plan
033, with a test). Conflicting cache env (any `SCCACHE_*` remote/multilevel
var, any `KACHE_*` S3 var in workflow env) → violation per the contract's
env rules.

**Verify**: `cargo nextest run -p velnor-runner backend` — new tests
`dual_cache_wrappers_rejected`, `remote_cache_env_rejected`.

### Step 2: Bake binaries into the job image

Add to the `docker/` job-image build: sccache **v0.16.0** and kache
**v0.10.0** release binaries (musl/static builds, both arches the image
supports), installed to `/usr/local/bin`. Pin by version AND sha256 of the
release artifacts (record the sha256s as build args in the Dockerfile — fetch
them at execution time from the upstream release pages and hardcode).

**Verify**: image build (operator-gated if no Docker locally); grep the
Dockerfile for both versions + sha256s.

### Step 3: Rewrite `native_sccache`; add `native_kache`

`native_sccache` (executor.rs:1945): validate inputs against the manifest
(033 gives the rule engine; provided-but-forbidden input → step failure with
the precise error). Replace the download script: new script asserts
`command -v sccache` (bail with "job image must ship sccache v0.16.0" if
absent — NEVER download), asserts `sccache --version` contains `0.16.0`,
exports the local env exactly as today (`SCCACHE_DIR`,
`SCCACHE_GHA_ENABLED=false`), sets `SCCACHE_CACHE_SIZE=${SCCACHE_CACHE_SIZE:-20G}`,
starts the server. Post step (`executor.rs:1836-1874` dispatch): emit the
stats report (`sccache --show-stats`) into the step log + `$GITHUB_STEP_SUMMARY`
— the adapter owns reporting.

`native_kache`: manifest-validated inputs (the five allowed, exact values);
setup script asserts baked binary + version 0.10.0, sets
`RUSTC_WRAPPER=kache`, mounts nothing new (store mount in step 4), never
touches GitHub cache or Node; post step emits kache's local report.
Register the adapter: `NativeActionAdapter::Kache` variant, `action.rs:137`
match arm for `kunobi-ninja/kache-action`, dispatch arms in
`executor.rs:1776-1826` (main) and `:1836-1874` (post), manifest entry with
the pinned commit `49398d37113c616fdb61be434cb497e3c2c8f3e6`.

**Verify**: tests `sccache_adapter_rejects_forbidden_input`,
`sccache_setup_never_downloads` (script contains no `curl`/`wget`),
`kache_adapter_sets_wrapper_and_reports` (model after
`native_tool_adapters_use_job_container_without_no_sidecars`,
`executor.rs:8422`). Suite green.

### Step 4: Separate stores + mounts

`container.rs`: mount the selected backend's store only — sccache keeps
`/var/cache/sccache` from `sccache_host()`; add `kache_host()` →
`<work-root>/_velnor_kache` mounted at kache's expected store path (read
kache v0.10.0's docs for its store env/path; set its store env var in the
container env alongside the mount). `Off` mounts neither. Backend selection
must reach `JobContainerSpec` (plumb through `github_job_container_spec`,
`runner.rs:3673`).

**Verify**: container arg tests (model `builds_start_container_args_with_mounts`,
`container.rs:1043`): sccache job has sccache mount and no kache mount;
kache job vice-versa; off job neither.

### Step 5: Contract status + fixture note

Annotate implemented gates in `docs/strict-capability-contract.md`
(`(closed: plan 034)` on gates 4–5 lines). Add a TODO pointer in the doc's
"Standard estate use" section that fixture backend-selection jobs land via
plan 041.

**Verify**: grep shows the annotations.

## Test plan

≥ 7 new tests as listed; full workspace gates green. Operator smoke (after
041 fixture jobs exist): fixture `off`/`sccache`/`kache` jobs green on the
Velnor lane; `both` lane green on GitHub for sccache/off (kache job runs
GitHub-side too per the contract's canary design — `github-cache: "false"`
keeps it local there).

## Done criteria

- [ ] Gates exit 0; new tests pass
- [ ] No job-time download path remains (`grep -n "releases/download" crates/velnor-runner/src/executor.rs` → none in cache scripts)
- [ ] v0.16.0/v0.10.0 baked + asserted; dual-wrapper + remote-env rejected
- [ ] Adapter post steps own reporting (stats in step log + summary)
- [ ] No out-of-scope changes

## STOP conditions

- kache v0.10.0's store layout/env cannot satisfy "local content-addressed
  store on a bind mount" (the storage doc flags a documented
  container/SQLite topology constraint) → STOP; record the exact constraint —
  kache stays a non-default canary until it passes.
- The image cannot ship both binaries for an arch the fleet runs → STOP.
- Existing estate workflows (java-monorepo rust.yml pre-plan-047) send
  `SCCACHE_GHA_ENABLED: "true"` — the seam must TOLERATE this specific env
  var by overriding it to false exactly as today's script does (compat until
  estate plans land), NOT reject it. If the contract's env rules seem to
  require rejecting it, STOP and get an operator ruling (rollout ordering
  problem).

## Maintenance notes

- Version bumps of either tool now = image rebuild + manifest version bump +
  fixture rerun. Renovate should watch both upstream repos.
- The 20 GiB equal budgets are for fair measurement; changing them versions
  the manifest (contract).
- Deferred: GHA/S3 remote modes (separate operator approval), multi-host
  store sharing (V2.1).
