# GitHub Action consumer-contract rereview — exact `9edac5e9`

## Disposition

**Changes required. Source-only review; no merge, rollout, macOS payload,
Docker operation, or host runtime approval.**

Reviewed exact revision
`9edac5e939ad361277db30ec95d0deb73b0edbed` on
`codex/github-first-action-scanner` from clean detached checkout
`/private/tmp/velnor-action-review-9edac5`.

Runner comparison uses `actions/runner`
`80bb1fb827fa44d489263061e71ef4adba7ad8cd` at
`/tmp/actions-runner-protocol-review`.

## Findings

1. **P1 — success and skip-build output assertions can silently skip.**
   `crates/velnor-workflow/tests/fixtures/github-action-consumer/workflow.yml:23-26`
   and `:67-72` put the only output/downstream checks behind
   `if: steps.action.outputs.result == 'downloaded'`. If the action succeeds
   with a missing or wrong output, the assertion step is skipped and the job
   remains green; the success fixture also does not positively assert that
   `hadolint` and `buildx` ran. Make the assertion unconditional/`always()`
   after an explicit action-success check, assert downstream success, and
   assert positive build markers in the non-skip case.

2. **P1 — consumer marker validation accepts a non-consumer graph.**
   `crates/velnor-workflow/src/s2/primitives/github_action.rs:134-145`
   uses line-prefix plus substring matching. A tracked fixture such as
   `uses: octo/__VELNOR_ACTION_PATH__` passes the marker check; replacement
   yields `octo/./`, and `validate_consumer_workflow_graph` (`:147-203`) sees
   no local action and accepts the workflow without invoking the scanned
   action. Parse the source YAML before replacement and require exact scalar
   `uses: __VELNOR_ACTION_PATH__` and exact checkout-marker scalars; add a
   negative embedded-marker test. The emitted fixture is real YAML now, but
   the render test's `contains` assertions (`:543-569`) would not catch this
   class.

## Prior `b52` finding closure

- **Checkout:** closed structurally. Fixture has a pinned checkout marker
  before local action steps (`workflow.yml:11-16`); renderer substitutes the
  immutable `Pins::checkout` reference and validates order
  (`github_action.rs:76-116,147-203`).
- **PR/push execution and watch:** closed structurally. Fixture declares both
  triggers (`workflow.yml:3-6`), and `consumer_workflows` enters the unit watch
  set (`github_action.rs:54-65`).
- **Failure/downstream/no-build matrix:** substantially closed. Generated jobs
  cover downloader, validator, hadolint, Buildx, and downstream failures
  (`workflow.yml:28-164`), suppress downstream on failure, and assert absent
  hadolint/Buildx in skip-build (`:67-72`). Finding 1 remains for success/skip
  assertion gating and positive build coverage.
- **Docker scheme/image parity:** closed for the reviewed paths. Scanner and
  runner use shared `velnor_model::action_reference::ActionImageReference`
  (`crates/velnor-workflow/src/s2/scan/action.rs:14,1505-1521`;
  `crates/velnor-runner/src/action.rs:781-801`), including case-insensitive
  `docker://` and OCI grammar rejection.
- **Missing composite `runs.steps`:** remains closed from `b52`; scanner
  presence check and regression test are at `action.rs:1212-1215,2456-2489`.

## Verification

Passed on the clean exact checkout:

- `rtk cargo test -p velnor-workflow --locked s2::scan::action::tests -- --nocapture` — **25 passed** (`19` suites; `1873` filtered).
- `rtk cargo test -p velnor-model --locked action_reference::tests -- --nocapture` — **3 passed**.
- `rtk cargo test -p velnor-model --locked` — **156 passed** (`5` suites).
- `rtk cargo test -p velnor-workflow --locked s2::primitives::github_action::tests -- --nocapture` — **13 passed** (`19` suites; `1885` filtered).
- `rtk cargo test -p velnor-runner --all-features --locked action_contract::tests -- --nocapture` — **7 passed** (`18` suites; `2517` filtered).
- `rtk cargo test -p velnor-workflow --locked` — **1898 passed** (`20` suites; `38.49s`).

The normal generator check was attempted from the correct crate directory:

```text
rtk cargo run --locked --manifest-path Cargo.toml -- --plain --check --default-branch main ../..
```

It did not reach zero-diff regeneration: the declared renderer pin
`fdeed261bd2247a38db6922a7726cd45d3d6f31e` is not provisioned with the
required closure. The binary reports the published/product closure mismatch
and requires `VELNOR_WORKFLOW_PINNED_BINARY` or local-only `--pin-build`.
This is an external verification blocker, not permission to publish or roll
out a new pin.

No source files were edited. Detached review checkout stayed clean at the
exact revision.
