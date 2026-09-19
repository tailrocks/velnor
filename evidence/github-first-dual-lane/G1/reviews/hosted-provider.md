# G1 hosted release-provider review

## Scope

- Candidate: `1c5eb2aab1c4785f7915e92b9fd1249ab978c626`
- Parent: `a38e459c7c88fa1c6e9a646a7513b2eecac292b8`
- Subject: `feat: make release verification lanes explicit`
- Review tree: detached `/private/tmp/g1-hosted-provider-review`
- Reviewed files: `.github-gen/velnor-workflow.toml`, `s2/config/mod.rs`, `s2/mod.rs`, `s2/primitives/release.rs`
- No source or generated workflow files were changed. Worktree stayed clean.

## Verdict

**Do not approve this exact commit.** The typed provider contract is sound and
the hosted-only surface is structurally close, but producer-bound preview has a
real dependency-context error, and the privileged producer admission does not
bind repository/source identity. Both need source-level fixes and regression
coverage before integration.

## What is correct

### Explicit provider contract

`ReleaseSection.verification_providers` is optional (`s2/config/mod.rs:350`).
Validation runs from `RepoGenerationConfig::validate` (`:1538`) and rejects an
empty set, duplicates, unknown IDs, and IDs outside `[workflow] providers`
(`:1750-1771`). `apply_release` converts the validated TOML array into the
typed `ProviderSet` (`s2/mod.rs:2218-2268`).

`release_verification_providers` uses the explicit set and otherwise falls back
to the complete provider universe (`s2/primitives/release.rs:2295-2318`). Thus
the new field does not silently change old configs: omitted means the normal
dual/provider-universe fanout.

### Hosted-only and one publisher

The candidate config declares `verification_providers = ["github-hosted"]`
while retaining `github-hosted, velnor` in the provider universe
(`.github-gen/velnor-workflow.toml:15-18, 67-83`). Stable and preview unit jobs
iterate only the selected set (`release.rs:3186-3210, 3315-3399`). Release-side
artifact matrices and the publisher still use `release_provider`, which is one
hosted writer whenever hosted is in the universe (`:2288-2309`); verification
selection does not create a second publisher. Existing focused coverage checks
that selected stable/preview jobs contain no `release-velnor-*` job or build
dependency (`:5653-5720`).

The scanned-surface test covers both explicit hosted selection and omission
fallback (`release.rs:5760-5809`). The generated config therefore gives hosted
recovery without hiding Velnor from normal CI or changing omitted legacy
release contracts.

### Verifier result gating

Stable build/publish jobs receive the exact selected verifier IDs. Native's
`native_release_build_gate` computes result clauses from the IDs actually
rendered (`release.rs:3499-3561`), so an omitted local lane is not accidentally
required. Preview build and the singular publisher also receive the same
selected IDs (`:2626-2701`, `:2054-2142`). With valid direct dependencies,
GitHub's normal failed/skipped dependency semantics prevent a publisher from
successfully bypassing a verifier. The structural tests pass for this part;
they do not execute hosted Actions runtime behavior.

## Blocking findings

### 1. Producer-bound verifier reads a non-direct `needs` output

**Location:** `s2/primitives/release.rs:3325-3342`.

For a producer-bound preview, the renderer emits:

```yaml
release-github-hosted-...:
  needs: [publish-gate]
  ref: ${{ needs.source.outputs.sha }}
  HEAD_SHA: ${{ needs.source.outputs.sha }}
```

`publish-gate` itself needs `source` (`:2444-2458`), but `source` is not a
direct dependency of the verifier. GitHub's `needs` context contains outputs
only from direct dependencies and excludes transitive dependencies:
[GitHub contexts reference](https://docs.github.com/en/enterprise-server%403.17/actions/reference/workflows-and-actions/contexts).
Therefore the verifier cannot reliably resolve `needs.source.outputs.sha` in
this graph; its checkout/HEAD binding is not proven and can fail or resolve
empty. The current test at `:5722-5755` passes because it asserts the broken
text (`needs: [publish-gate]` plus `needs.source...`) rather than validating
the dependency context.

**Narrow fix:** make `source` a direct verifier dependency:
`needs: [source, publish-gate]`, retaining the admission gate. This is acyclic:
`source -> publish-gate -> verifier`, with `source` also directly required by
build; verifier -> build -> publisher. Alternatively pass the SHA through
`publish-gate.outputs.sha` and use only its direct output, but the test must
still assert every `needs.<job>.outputs` reference has that job in the same
job's direct `needs` list.

**Required regression:** render a bound preview, parse job blocks, assert the
verifier's direct needs contain both `source` and `publish-gate`, assert the
checkout ref and both `HEAD_SHA` occurrences use the admitted SHA, and run a
small DAG check that rejects cycles and transitive-only output references.

### 2. Producer admission is not bound to repository/source identity

**Location:** source/gate rendering `s2/primitives/release.rs:2431-2464`;
runtime `s2/runtime.rs:3923-3960`.

`resolve-source` chooses `workflow_run.head_sha` and validates only that it is
40 hexadecimal characters. `admit-producer` checks only workflow name equality
and `conclusion == success`. No admission input or check binds:

- `github.event.workflow_run.repository.full_name` to `github.repository`;
- triggering workflow event/ref/head branch to the intended trusted source;
- the producer run identity/ID and source SHA to the admitted run; or
- a producer artifact/manifest identity when the source is consumed.

The configured `.github-gen` release currently has no producer binding, so this
gap is latent there; it is active in the generic producer-bound preview path
and directly affects the required fork/trust/source-identity contract. A
`workflow_run` workflow is privileged and can access secrets/write tokens even
when the preceding workflow cannot; GitHub explicitly warns that checking out
untrusted code on this trigger risks cache poisoning and unintended privilege:
[workflow_run security warning](https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows).

**Narrow fix:** extend the source admission contract to carry and validate
repository full name, producer workflow identity, event/ref/head branch, run ID,
and head SHA before publishing `source.outputs.sha`; require the exact trusted
same-repository/default-branch producer shape intended by this release. Keep
the SHA format check, then only expose the output after all identity checks
pass. Add negative tests for fork repository, wrong event, wrong branch/ref,
wrong run identity, and mismatched SHA, plus the existing name/conclusion
matrix. Do not treat the workflow name and `success` alone as source trust.

## Checks performed

All commands ran in the detached candidate tree; no generated files were
written.

- `rtk cargo fmt --check --manifest-path crates/velnor-workflow/Cargo.toml` — pass.
- `rtk cargo clippy -p velnor-workflow --all-targets -- -D warnings` — pass.
- `rtk cargo test -p velnor-workflow release_verification_providers_are_explicit_and_bounded -- --nocapture` — 1 passed.
- `rtk cargo test -p velnor-workflow explicit_release_verification_lanes_gate_stable_and_preview -- --nocapture` — 1 passed.
- `rtk cargo test -p velnor-workflow producer_bound_preview_verification_uses_admitted_source_sha -- --nocapture` — 1 passed (test is insufficient; see finding 1).
- `rtk cargo test -p velnor-workflow parsed_release_contract_selects_exact_verification_lanes_in_surface -- --nocapture` — 1 passed.
- Runtime source/admission tests — 2 passed each for `resolve_source_binds_the_producer_revision`, `admit_producer_refuses_name_and_conclusion_mismatch`, and `resolve_mode_refuses_untrusted_publish`.
- `rtk cargo run -q -p velnor-workflow -- --plain --dry-run` — command completed without writes; reports the expected source/generated drift: **12 files would change**. This is not generated-output approval.
- `rtk git diff --check a38e459c7c88fa1c6e9a646a7513b2eecac292b..HEAD` — pass.

The candidate's focused test/lint claims are reproducible, but they cannot
override the dependency-context and source-identity findings above.
