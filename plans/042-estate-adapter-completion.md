# Plan 042: Estate adapter completion — Pages, attest/cosign, local composites, secret-login gating

> **Executor instructions**: Follow step by step; run every verification; STOP
> conditions binding. Update the status row in `plans/README.md` when done.
>
> **Drift check (run first)**:
> `git diff --stat 48b04ad..HEAD -- crates/velnor-runner/src/action.rs crates/velnor-runner/src/executor.rs`
> Re-locate by symbol; mismatch → STOP.

## Status

- **Priority**: P0/P1 (V0.4 + V0.6 + V1.1 + V1.2 + V1.5 — estate lanes fail
  without them; jackin/termrock/parallax block on Pages + attest)
- **Effort**: L
- **Risk**: MED
- **Depends on**: plans/033 (manifest entries for every touched action)
- **Category**: parity
- **Planned at**: commit `48b04ad`, 2026-07-18

## Why this matters

Estate migration (plans 047–057) runs these workflows on the Velnor lane by
default. The audit of the 13 repos' `uses:` inventory vs the adapter table
(`action.rs:137-163`, 22 adapters) leaves these gaps on the product path:
GitHub Pages family completeness (`configure-pages` has NO adapter;
deploy-pages/upload-pages-artifact exist but estate-grade parity is marked
partial), provenance/signing (`actions/attest-build-provenance` — no
adapter; `sigstore/cosign-installer` exists), `EmbarkStudios/cargo-deny-action`
(tablerock, pre-055), and local composite actions (`./.github/actions/*` —
jackin has 9, parallax 3) which must always resolve without the network.
Docker Hub secret login exists (`native_docker_login`) but must be
trust-gated per V0.6.

## Current state (from the verified adapter map; re-locate by symbol)

- Adapter table `action.rs:137-163`; handlers `executor.rs:1776-1826` (main)
  / `:1836-1874` (post). Existing relevant: UploadPagesArtifact,
  DeployPages, CosignInstaller, DockerLogin, Renovate, SetupQemu.
- MISSING from the table (estate sweep 2026-07-18): `actions/configure-pages`
  (termrock docs.yml), `actions/attest-build-provenance` (jackin/parallax
  sign-and-attest composites — VERIFY: grep those composites for the actual
  `uses:`; the estate table in `VELNOR_PROJECTS_SETUP.md` §3.12 flags
  attest as "gap — verify/native"), `EmbarkStudios/cargo-deny-action`
  (tablerock — dies anyway in plan 057; add to the DENYLIST instead,
  `unsupported_action_error` `action.rs:171`, message pointing at mise
  cargo-deny).
- Local composites: composite runtime exists (`ActionRuntime::Composite`
  arm, `runner.rs:4553` area; `parse_repository_uses` `action.rs:926`).
  V1.5's requirement: `./.github/actions/*` resolution never fetches and
  works identically on both lanes — needs a regression test per composite
  feature the estate uses (inputs, nested steps, `runs.using: composite`
  with `post`).
- Trust gating precedent: docker socket via
  `github_trust_scope_allows_host_docker` (`github_adapter.rs:100-102`);
  secrets policy `validate_job_trust_policy` (`runner.rs:2528`).

## Commands you will need

| Purpose | Command | Expected |
|---------|---------|----------|
| Gates | fmt / clippy / `cargo nextest run --workspace --locked` | exit 0 |
| Estate uses sweep | `for r in <13 repo paths from VELNOR_PROJECTS_SETUP.md §13>; do grep -rh "uses:" $r/.github/workflows $r/.github/actions 2>/dev/null; done | sed 's/@.*//' | sort -u` | the authoritative gap list |

## Scope

**In scope**: `action.rs` (table + denylist), `executor.rs` (new/completed
handlers + trust gating), manifest entries (033's `manifest.rs`), tests,
`docs/reference/target-action-registry.md` sync.
**Out of scope**: fixture jobs for the new adapters (append to plan 041's
follow-up — note in status row); estate YAML; the JS sidecar (unavailable on
the product path after 033).

## Git workflow

Branch `velnor-estate-standard`; `feat(runner):` commits per adapter,
`git commit -s`; no push without operator instruction.

## Steps

### Step 1: Authoritative gap list

Run the sweep command; diff against `native_action_adapter` matches +
denylist. Output: table (action → repos using it → adapter status →
decision: native / denylist / composite-covered). Paste into the PR.

**Verify**: table exists; every estate `uses:` classified.

### Step 2: `actions/configure-pages` adapter

Mirror upstream behavior (actions/runner + the action's source — it mostly
resolves the Pages site config via the GitHub API and exports outputs like
`base_url`). Implement `native_configure_pages` with the real output
surface; manifest entry; tests (pattern:
`native_cache_reports_miss_without_node_sidecar`, `executor.rs:7312`).

**Verify**: focused nextest green.

### Step 3: Pages deploy parity pass (V1.2)

Exercise UploadPagesArtifact + DeployPages handlers against the current
GitHub Pages deployment API flow (upstream source truth); fix drift found
(the estate table calls it "partial — complete for jackin/termrock").
Document verified behavior in the registry doc.

**Verify**: updated/new tests for the full artifact→deployment status loop
(wiremock the API endpoints — harness `tests/broker_protocol.rs` pattern).

### Step 4: attest-build-provenance decision + adapter

If step 1 confirms estate usage: implement natively (Sigstore signing of
subject digests via the GitHub attestation API) — LARGE; if the flow proves
infeasible natively this cycle, the documented fallback (per V1.1
"native or documented") is: manifest-reject with a precise error naming the
GitHub-lane-writer workaround, and the estate keeps attest steps
writer-gated to the GitHub lane (plan 053 already does). Either way the
decision is explicit in the registry doc — silent sidecar execution is not
an option (033 removed it).

**Verify**: adapter tests OR the documented rejection + registry entry.

### Step 5: cargo-deny-action → denylist; secret-login trust gate (V0.6)

Add `EmbarkStudios/cargo-deny-action` to `unsupported_action_error` with a
"use mise cargo-deny" message. In `native_docker_login` (`executor.rs:2333`):
refuse secret-bearing login when trust scope ≠ trusted (reuse the
`validate_job_trust_policy` machinery/message style) — registry-credential
secrets must never reach non-trusted pools.

**Verify**: tests `cargo_deny_action_denylisted`,
`docker_login_refused_outside_trusted_scope`.

### Step 6: Local-composite regression net (V1.5)

Inventory composite features used by jackin/parallax composites (step 1
sweep covers `.github/actions`); add regression tests per feature (inputs
with defaults, multi-step run/uses nesting, outputs, post) driving
`ActionRuntime::Composite` through the executor test harness.

**Verify**: focused nextest green; each estate composite feature named in a
test.

## Test plan

Per-step tests (≥ 8 new); full gates. Estate-level acceptance happens via
estate plans' V-B lane-parity dispatches.

## Done criteria

- [ ] Gates exit 0; step-1 table in PR; every estate action classified
- [ ] configure-pages native; Pages loop verified; attest decided +
      implemented-or-documented; deny-action denylisted; login trust-gated
- [ ] Composite regression tests cover estate features
- [ ] Manifest + registry doc updated; no out-of-scope changes

## STOP conditions

- The attestation API requires OIDC-token flows the runner cannot mint
  outside GitHub-hosted → that CONFIRMS the documented-fallback branch of
  step 4; if even the fallback is unclear, STOP.
- Step-1 sweep finds an estate action family not in this plan (unknown
  unknowns) → add to the table, classify, and if native work exceeds this
  plan's scope, file it in the status row and continue with the rest.

## Maintenance notes

- Every new estate action from now on enters through: manifest entry →
  fixture job (041 pattern) → adapter — reviewers enforce the order.
- Follow-up (not here): fixture jobs for configure-pages + attest once
  adapters land.
