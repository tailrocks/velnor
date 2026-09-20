# Exact checker review: `27f22303f76c9f59c3909215d51c8135e0bc3b64`

## Bounded verdict

**Changes required.** This is a read-only source review of the exact checker
tree, not live-authority, G0, or rollout approval. No checker source or
producer output was changed.

The commit correctly separates tagged GitHub Actions and ExternalApp URL
forms and rejects unknown provider fields, but it does not yet establish the
required provider-object bindings or safe ExternalApp checkout semantics.

## Findings

### F1 — Actions job URL shape is not the captured provider shape (high)

`crates/velnor-tools/src/evidence_check.rs:2599-2615` requires
`/{repository}/runs/{workflow_run_id}/jobs/{job_id}` for
`job_html_url` (called at `:3766`). Captured GitHub Actions check URLs use
`/{repository}/actions/runs/{workflow_run_id}/job/{id}`; for example,
`G0/fleet/check-contexts-full.json` (SHA-256
`1a51f8a276c912c6f03f3bcb749d1f5845b52c0551cdff6254c8e90c0c78901e`) records
this form. The positive fixture manufactures the non-captured form at
`evidence_check.rs:7951-7973`, so the real Actions provider shape is not
tested and is likely rejected. Preserve and bind the actual API URL/object;
add captured Actions positives and wrong-run/attempt/job mutations.

### F2 — ExternalApp accepts invented checkout proof (high)

`evidence_check.rs:3819-3822` and `:4069-4071` require every provider,
including `ExternalApp`, to carry `actual_checkout_sha`. The ExternalApp
snapshot paths then match that caller-supplied SHA to an unrelated Actions
execution at `:3901-3912` and `:4207-4218`. The fixture does this explicitly
at `:8215-8224`. Real DCO-2 checks have no workflow-run IDs or checkout
attestation. External checks must not infer checkout from an Actions run;
model unavailable checkout separately or require an independent provider
attestation, and add fabricated/missing-checkout mutations.

### F3 — App, suite, raw object, and job membership are not strictly bound
 (high)

`g0_contract.rs:331-366` stores only `app_id`/`app_slug` for the App and an
untyped `ExternalApp`; `evidence_check.rs:3733-3746` only rejects an empty
slug or `github-actions`. A nonempty fake external slug would pass. Suite
IDs are only checked nonzero (`:3813-3826`, `:4064-4075`).

`check_g0_raw_refs` (`:2251-2268`) checks only that referenced IDs exist; it
does not verify object kind, endpoint, or response bytes. Snapshot check
validation (`:5072-5089`) does not require App identity or job membership,
and the provider/snapshot joins (`:3890-3899`, `:4196-4205`) never join the
check to `execution.jobs`. Bind captured App ID/slug/URL, check-suite,
check-run, workflow-run/attempt, and job objects; require job membership and
reject wrong-App, wrong-suite, duplicate, not-in-jobs, and raw-object
mutations.

### F4 — Positive coverage is synthetic, not captured-provider evidence

`complete_g0_fixture` uses fabricated IDs/URLs, app ID `123`, and generic
`object_kind = "github-response"` (`evidence_check.rs:7785-7807,
:7951-7973`). It does not use captured DCO-2 (`974774/dco-2`), SonarQubeCloud
(`12526/sonarqubecloud`), and GitHub Actions (`15368/github-actions`) API
objects. Replace/add positives from captured objects and mutate their typed
and raw bindings.

## Verification

- `rtk cargo test -p velnor-tools` — **254 passed**.
- Checker worktree at the reviewed commit was clean.
- No live dispatch, provider mutation, rollout, or gate claim was made.
