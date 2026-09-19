# Current-main reconciliation for PR955

Observed: 2026-09-19T20:40:00Z UTC

## Revisions

- GitHub `main`: `0dc79895ff1c5e88be7c3822c437e1c5b5282e12`
- PR955 base recorded by GitHub: `e713841bdb9c33d853b7a9af88ceac924af1b3b6` (stale relative to `main`)
- PR955 head: `15c8110e7b5330ebe58f47a0bded5388c47bc4c5`
- PR955 head merge-base: `e713841bdb9c33d853b7a9af88ceac924af1b3b6`

`main` advanced by the verified squash merge of PR954 after the e713 pin-adoption commit was pushed. The current-main delta from e713 is:

- `.github/ci/.github-actions-generator-state`
- `crates/velnor-workflow/src/s2/primitives/mod.rs`
- new `crates/velnor-workflow/src/s2/primitives/package_release.rs`
- `crates/velnor-workflow/src/s2/primitives/release.rs`

Current `main` still declares the old fdeed generator pin and its checked-in workflows retain the admitted `a20e1ffc...` action revision. A pin adoption based only on e713 therefore cannot be accepted as current-main G1 evidence.

## Newly published current-main runtime

The mainline producer ran after PR954 merge and published the current product:

- producer [run 35467711423](https://github.com/tailrocks/velnor/actions/runs/35467711423), head exactly `0dc79895ff1c5e88be7c3822c437e1c5b5282e12`, all five jobs passed
- release [velnor-workflow-runtime-v1-8b96d5108550dfa6](https://github.com/tailrocks/velnor/releases/tag/velnor-workflow-runtime-v1-8b96d5108550dfa6), immutable, published `2026-09-19T20:36:26Z`, tag target exactly current main
- closure: `8b96d5108550dfa61a57ff65c6c357b4119493c660417bf3beb4af2b03742269`
- manifest SHA-256: `a13b01e06421faf616bc5a8095bd815a6dc73b4c8c7ef00a9c147c9ffa7691d9`
- Linux-ARM64 binary: `31573446bc44f5be82ba75ec6413d74a66987cf00e1c31715e74bae0a52a4993`
- Linux-X64 binary: `915e8112c0f5307741cc6c58b6907d5cfaf6488f23c56e6e8dc78d1ff9f545d1`
- macOS-ARM64 binary: `1bba241e5c13a90b307233808b845230117a571a3c645b8b1c9849e6ceafcc82`

Downloaded manifest and all three assets were rehashed. `gh attestation verify` passed for each asset with signer workflow `ci-runtime-products.yml` and source ref `refs/heads/main`.

## Renderer reconciliation

A detached checkout of current main with only its authoritative config pin changed from fdeed to current main `0dc79895...` was rendered by the published macOS-ARM64 product from the 0dc release. The renderer changed only the expected pin-dependent fields and generator sidecar:

- `.github/ci/.github-actions-generator-state`
- `ci-main.yml`, `ci-policy.yml`, `ci-pr.yml`
- all `ci-unit-*` support workflows
- `maintenance.yml`, `preview.yml`, `release.yml`

No source-owned output or provider configuration was invented. The current-main source changes are already represented in the 0dc product; the exact output must be regenerated after integrating current main into PR955.

## Required disposition

1. Do not accept PR955 commit `15c8110e` as current-main G1 proof; it pins e713 while main is now 0dc.
2. Reconcile PR955 with current main `0dc79895...` using the already-published 0dc product, not a local runtime build.
3. Regenerate from the authoritative 0dc renderer, then rerun byte-match, drift, actionlint, runner admission, policy, review, and the complete hosted required matrix.
4. Keep the e713 runtime evidence source-bound to the former base; it is valid but not current-main evidence.

The in-progress policy run [35467815794](https://github.com/tailrocks/velnor/actions/runs/35467815794) is still blocked in `Acquire candidate generator product` for the stale e713-based head and must not be treated as a current-main result.
