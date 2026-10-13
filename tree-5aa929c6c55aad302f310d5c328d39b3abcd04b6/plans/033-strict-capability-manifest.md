# Plan 033: Strict capability manifest — typed ref/input validation, no unknown-action fallback

> **Executor instructions**: Follow this plan step by step. Run every
> verification command before moving on. On any STOP condition, stop and
> report. When done, update this plan's status row in `plans/README.md`.
>
> **Drift check (run first)**:
> `git diff --stat 48b04ad..HEAD -- crates/velnor-runner/src/action.rs crates/velnor-runner/src/runner.rs crates/velnor-runner/src/executor.rs crates/velnor-runner/src/cli.rs`
> If these changed since `48b04ad`, re-verify every "Current state" excerpt
> (line numbers may shift — re-locate by symbol name with grep); on a
> semantic mismatch, STOP.

## Status

- **Priority**: P0 (V0.14; strict-capability-contract gates 1–3, 6, 7)
- **Effort**: L
- **Risk**: HIGH (touches the dispatch path of every job)
- **Depends on**: none (first runner plan; 034 builds on its seam)
- **Category**: security / correctness
- **Planned at**: commit `48b04ad`, 2026-07-18

## Why this matters

`docs/strict-capability-contract.md` (in-repo; READ IT FIRST — it is the
requirement source) is accepted law: Velnor supports only behavior declared
in a versioned Rust capability manifest, validates the complete job before
side effects, and fails unsupported configuration precisely. Today none of
that exists: adapters dispatch on repository name alone with the commit ref
discarded, adapter inputs have no allowlist (the sccache adapter ignores
inputs entirely), and unknown JavaScript actions silently execute in a Node
sidecar. Each is an enabling structure for silent divergence from GitHub
behavior — the exact bug class the contract exists to remove.

## Current state (all excerpts verified against `48b04ad`)

- `crates/velnor-runner/src/action.rs:137` — `native_action_adapter(repository: &str)`:
  a `match repository.to_ascii_lowercase()` over 22 action repos returning
  `Option<NativeActionAdapter>` (enum at `action.rs:112-135`). No ref checked.
- `action.rs:300-304` — a missing ref is synthesized for native actions:
  ```rust
  let git_ref = reference.git_ref.clone()
      .or_else(|| native_action_adapter(repository).map(|_| NATIVE_ACTION_REF.to_string()))
      .ok_or_else(|| anyhow::anyhow!("repository action '{repository}' missing ref"))?;
  ```
  (`NATIVE_ACTION_REF = "__native"`, `action.rs:227`; codified by test
  `native_repository_action_plan_does_not_require_ref`, `action.rs:1577`.)
- `action.rs:669-675` — `native_invocation_from_plan` maps repo → adapter,
  copying `plan.inputs` verbatim; no input validation anywhere.
- `crates/velnor-runner/src/executor.rs:1945-1954` — `native_sccache` binds
  the invocation as `_action` (inputs ignored) and runs a fixed script.
- `crates/velnor-runner/src/runner.rs:4553` (`append_resolved_action_steps`)
  — runtime selection: native adapter first, then a 2-entry denylist
  (`unsupported_action_error`, `action.rs:171` — `dtolnay/rust-toolchain`,
  `baptiste0928/cargo-install`), then **any** unknown JS action falls through
  to a Node sidecar (`ExecutableStep::JavaScript`, sidecar exec at
  `executor.rs:1608`, image default `node:24-bookworm`).
- Preflight seam — `runner.rs:2374-2392` (verified): job parsed at
  `runner.rs:2236`, the only pre-side-effect job check is
  `validate_job_trust_policy(&job, &args.trust_scope)?` at `runner.rs:2377`,
  then `spawn_blocking(execute_script_job)` at `:2388`. Side effects happen
  inside: checkout `runner.rs:3607`, action download `:3652`, container spec
  `:3673`.
- No `capabilities` CLI: `velnor-runner` `Command` enum (`cli.rs:13-32`) has
  `Cache, Configure, Daemon, Preflight, Run, Remove, Status, Doctor`.
  `RunArgs.dump_job_message` (`cli.rs:202`) already emits a sanitized job
  dump (writer `write_sanitized_job_message_dump`, used near `runner.rs:2304`)
  — the input format for `capabilities check`.
- Doc registry (no code counterpart): `docs/reference/target-action-registry.md`.
- Estate reality check (from the fixture + estate audits): every estate
  workflow SHA-pins its actions, so enforcing approved commits is feasible —
  but the manifest must list the CURRENT estate pins (see Step 2) or the
  rollout bricks every repo.

Repo conventions: errors via `anyhow` with precise, user-visible messages
(match the style of `unsupported_action_error`, `action.rs:171`); tests
inline `#[cfg(test)]` per module; run gates from `mise.toml`
(`mise run fmt` / `lint` / `test`).

## Commands you will need

| Purpose | Command | Expected |
|---------|---------|----------|
| Format | `cargo fmt --all --check` | exit 0 |
| Lint | `cargo clippy --workspace --all-targets --locked -- -D warnings` | exit 0 |
| Tests | `cargo nextest run --workspace --locked` | all pass |
| Focused | `cargo nextest run -p velnor-runner manifest` | new tests pass |

## Scope

**In scope**:
- `crates/velnor-runner/src/manifest.rs` (new)
- `crates/velnor-runner/src/action.rs`, `runner.rs`, `executor.rs` (dispatch
  + preflight wiring), `cli.rs` + `main.rs` (new subcommand)
- `docs/strict-capability-contract.md` (status note only: mark implemented
  items), `docs/reference/target-action-registry.md` (point at the manifest)

**Out of scope**:
- The sccache/kache backend seam internals (plan 034) — this plan only makes
  the sccache adapter VALIDATE inputs; behavior change is 034's.
- Removing the JS sidecar CODE (keep it callable behind the diagnostic flag —
  see Step 4); deleting it entirely is a later cleanup.
- Estate workflow changes (estate plans 047–057).

## Git workflow

Branch `velnor-estate-standard`; conventional commits (`feat(runner): ...`),
`git commit -s`; no push without operator instruction.

## Steps

### Step 1: The manifest type

New `crates/velnor-runner/src/manifest.rs`:

```rust
pub struct CapabilityManifest { pub version: u32, pub actions: Vec<ActionCapability> }
pub struct ActionCapability {
    pub repository: &'static str,            // lowercase
    pub adapter: NativeActionAdapter,
    pub allowed_refs: &'static [AllowedRef], // full 40-char commit SHAs + the tag comment; empty slice = ref-free actions (see below)
    pub inputs: &'static [InputRule],        // name, requirement, allowed values
    pub notes: &'static str,                 // upstream source commit / fixture case
}
pub enum InputRule { Any(&'static str), Literal(&'static str, &'static [&'static str]), Forbidden(&'static str) }
```

Compile it as a `static` (or `LazyLock`) — pure data, versioned by a bumped
`version` on every surface change. Include a `to_json()` for export.
Populate it from THREE sources, in this order of authority:
1. `docs/strict-capability-contract.md` — sccache: only
   `version=v0.16.0`, `disable_annotations=false` allowed, `token` may not be
   supplied; kache entry per its section (adapter arrives in 034 — give the
   kache entry `adapter: NativeActionAdapter::Kache` only if 034 landed
   first, else omit and leave a `// 034` comment).
2. The estate pin sweep (allowed commits — REAL SHAs from the estate repos,
   e.g. checkout `9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0` (v7) and
   `df4cb1c069e1874edd31b4311f1884172cec0e10` (v6, until estate plan 052/053
   bump), cache `55cc8345863c7cc4c66a329aec7e433d2d1c52a9` (v6) and
   `27d5ce7f107fe9357f9df03efb73ab90386fccae` (v5), mise-action
   `e6a8b3978addb5a52f2b4cd9d91eafa7f0ab959d` (v4.2.0),
   `dad1bfd3df957f44999b559dd69dc1671cb4e9ea` (v4.2.1),
   `dba19683ed58901619b14f395a24841710cb4925` (v4.1.0), sccache-action
   `9e7fa8a12102821edf02ca5dbea1acd0f89a2696` and
   `1583d6b38d7be47f593cb472781bbb21cab4321e` (v0.0.10), setup-mold
   `9c9c13bf4c3f1adef0cc596abc155580bcb04444` (v1) — RE-SWEEP the estate at
   execution time with
   `grep -rh "uses:" <repo>/.github/workflows | sort -u` over the 13 repos
   listed in `VELNOR_PROJECTS_SETUP.md` §13; do not trust this list blindly).
3. Each adapter's actually-read inputs (grep `native_input` calls per handler
   in `executor.rs`) — every input a handler reads gets a rule; every input
   the upstream action defines but the handler ignores is `Forbidden` until
   the handler honors it (contract: an ignored provided input is a failure).

Fixture caveat: the fixture workflows use floating tags (`@v4`, `@v0.0.10`)
and `actions/checkout` steps there resolve refs the manifest must also allow
— sweep `velnor-actions-fixture/.github/workflows` too, and treat
un-SHA-pinned fixture `uses:` as work for plan 041 (the fixture migrates to
pinned refs there; until then the manifest carries the fixture's tag refs as
allowed refs with a `// until plan 041` note).

**Verify**: `cargo nextest run -p velnor-runner manifest` — add
`manifest_covers_every_native_adapter` (every `NativeActionAdapter` variant
except Checkout has a manifest entry) and `manifest_exports_json`.

### Step 2: Validation function + wiring at the preflight seam

New `manifest::validate_job(job, script_steps, trust_scope) -> Result<(), CapabilityViolation>`
walking every step of the expanded job: for each `uses:` step, repository
must be in the manifest, its resolved commit/ref in `allowed_refs`
(ref-free = only the synthesized `__native` path for actions the estate uses
un-reffed — keep it ONLY for `actions/checkout`, matching `action.rs:297`'s
special-case; all others require a ref), every provided input matched against
`InputRule`s, forbidden inputs rejected. Error type carries: step name,
repository, ref, field, received value, accepted alternatives, manifest
version — formatted as one precise message (contract format).

Wire it at `runner.rs:2377`, directly after `validate_job_trust_policy`:

```rust
validate_job_trust_policy(&job, &args.trust_scope)?;
crate::manifest::validate_job(&job, &script_steps, &args.trust_scope)?;
```

This is BEFORE `execute_script_job` (`:2388`), hence before checkout
(`:3607`), action download (`:3652`), and container creation (`:3673`) —
the contract's required ordering. Also call it from the `Run` (single-job)
path if that path bypasses `handle_job_request` — trace `run()` at
`runner.rs:505` to confirm both entries are covered.

**Verify**: new tests — `validate_job_rejects_unknown_repository`,
`validate_job_rejects_unapproved_ref`, `validate_job_rejects_forbidden_input`
(sccache `token`), `validate_job_accepts_estate_shaped_job` (build a job from
an existing test fixture in `runner.rs`/`github_adapter.rs` tests). Full
suite green.

### Step 3: Ref is no longer discarded

In `action.rs`, thread the resolved ref into `RepositoryActionPlan` (it
already has the data — `repository_dir(actions_host, repository, &git_ref)`
at `:305` uses it) and into `NativeActionInvocation` so step 2's validation
and 034's version pinning can key on it. Remove the blanket synthetic-ref
fallback at `action.rs:300-304` for everything except `actions/checkout`;
update test `native_repository_action_plan_does_not_require_ref` to assert
the NEW contract (checkout-only).

**Verify**: `cargo nextest run -p velnor-runner action` green after the
test update; `grep -n "NATIVE_ACTION_REF" crates/velnor-runner/src/action.rs`
shows checkout-scoped usage only.

### Step 4: Unknown-action fallback off the product path

In `append_resolved_action_steps` (`runner.rs:4553`): after this plan,
manifest validation (step 2) has already rejected unknown `uses:` before
execution, so the JS fallthrough is dead on the product path. Make that
explicit: gate the `ActionRuntime::JavaScript` arm behind a new
`--diagnostic-node-sidecar` flag (env `VELNOR_DIAGNOSTIC_NODE_SIDECAR`,
default off) that ALSO requires the manifest check to have been explicitly
skipped (`--skip-capability-validation`, same diagnostic tier). Without the
flags, reaching the JS arm is `bail!("unknown action '{repo}' reached execution — capability validation must reject this earlier")`.

**Verify**: test `unknown_javascript_action_fails_without_diagnostic_flag`;
existing sidecar test `executes_javascript_action_with_inputs_and_command_files`
(`executor.rs:11135`) updated to set the diagnostic flag. Suite green.

### Step 5: `capabilities` CLI

Add `Command::Capabilities { check { job_dump: PathBuf } | export }` to
`cli.rs`/`main.rs`: `check` loads a `--dump-job-message` JSON and prints
every violation (exit 1 if any); `export` prints the manifest JSON. Mirror
the clap patterns of `Command::Cache` (`cli.rs:16,35-58`).

**Verify**: `cargo run -p velnor-runner -- capabilities export | python3 -m json.tool`
→ valid JSON containing `"version"`; `capabilities check` on a dump produced
by an existing test fixture → exit 0.

### Step 6: Docs status sync

In `docs/strict-capability-contract.md`, under "Current architectural gaps",
annotate each closed gap with `(closed: plan 033)`. Point
`docs/reference/target-action-registry.md` at `manifest.rs` as the code
authority.

**Verify**: `grep -n "closed: plan 033" docs/strict-capability-contract.md` ≥ 3.

## Test plan

New tests listed per step (≥ 8 new). Structural pattern: the existing inline
module `action.rs:1370+` and `executor.rs:6415+` tests. Full gates:
`cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo nextest run --workspace --locked`.

## Done criteria

- [ ] All gates exit 0; ≥ 8 new tests pass
- [ ] `capabilities export`/`check` work as specified
- [ ] Unknown JS action cannot execute without both diagnostic flags
- [ ] Manifest validated BEFORE checkout/download/container (assert by test
      that a violating job produces no `_work` side effects — model after the
      RecordingRunner executor tests)
- [ ] No out-of-scope files modified

## STOP conditions

- The estate pin re-sweep finds `uses:` patterns with expression-computed
  refs (`@${{ ... }}`) → STOP; the contract needs an operator ruling.
- Wiring at `runner.rs:2377` cannot see resolved action refs because plans
  are built later (inside `execute_script_job`) → do NOT move side effects;
  instead run plan-building (pure parsing, `repository_action_plans`) before
  the spawn_blocking as part of validation, keeping downloads where they are.
  If that refactor exceeds ~100 lines, STOP and report the seam mismatch.
- Any fixture workflow fails `capabilities check` for a pattern the fixture
  legitimately covers → STOP; the manifest is wrong, not the fixture
  (fixture-is-contract).

## Maintenance notes

- Every new estate action or version bump now REQUIRES a manifest entry +
  version bump — this is the designed friction; reviewers must reject
  "temporarily allow any ref" changes.
- Plan 034 (backend seam) and 041 (fixture) both extend this manifest; land
  033 first.
- Deferred: deleting the JS sidecar code entirely once the estate soaks green.
