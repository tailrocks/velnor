# TreeA authority closure inventory

Observed: 2026-09-20T00:27:10Z UTC
Repository: `tailrocks/velnor`
Authority ref: `refs/heads/main`
Remote main: `d20d4d1d17590cca85b501d982cbaad70d42c641`
Commit/tree: `d20d4d1d17590cca85b501d982cbaad70d42c641` / `ec2d82aa83a1b2db692d1d3d46d48568d756c602`

## Verdict

**BLOCKED_FOR_CLOSURE_CLEAN.** Active config/generated workflows still pin old renderer `0dc79895ff1c5e88be7c3822c437e1c5b5282e12` at `.github-gen/velnor-workflow.toml:9`. Exact old-revision scan: **82 hits in 12 active files**. Exact old closure `8b96d5108550dfa61a57ff65c6c357b4119493c660417bf3beb4af2b03742269` and literal tag `velnor-workflow-runtime-v1-8b96d5108550dfa6` each have **0 textual hits**. This is not closure-clean: setup computes the tag from the pinned revision closure at source/generated setup action line 115.

No G1 gate, merge-readiness, or overall-green claim.

## Exact old hit map

- `.github-gen/velnor-workflow.toml`: 9
- `.github/workflows/ci-main.yml`: 66, 68, 86, 105, 154, 162, 264
- `.github/workflows/ci-policy.yml`: 62, 70, 172
- `.github/workflows/ci-pr.yml`: 65, 67, 85, 104
- `.github/workflows/ci-unit-bun.yml`: 172, 177, 191
- `.github/workflows/ci-unit-docker.yml`: 172, 177, 191
- `.github/workflows/ci-unit-docs.yml`: 172, 177, 191
- `.github/workflows/ci-unit-opentofu.yml`: 172, 177, 191
- `.github/workflows/ci-unit-rust.yml`: 223, 228, 242, 761
- `.github/workflows/maintenance.yml`: 125, 128
- `.github/workflows/preview.yml`: 207, 209, 490, 492, 631, 633
- `.github/workflows/release.yml`: 73, 75, 117, 119, 164, 166, 207, 209, 252, 254, 302, 304, 457, 459, 589, 591, 721, 723, 853, 855, 985, 987, 1117, 1119, 1249, 1251, 1381, 1383, 1513, 1515, 1645, 1647, 1777, 1779, 1909, 1911, 2674, 2969, 2971, 3377, 3379, 3732, 3734

Historical renderer supplied by owner: `e713841bdb9c33d853b7a9af88ceac924af1b3b6`, tree `0f16adfd1fce39c03568ff6d02e5f58c3580db7e`, tag `velnor-workflow-runtime-v1-8099c429155da548`; no literal hit on current main. Current remote main product tag: `velnor-workflow-runtime-v1-917c5421fa9bc4ef -> d20d4d1d17590cca85b501d982cbaad70d42c641`.

## Producer/consumer paths

- Runtime producer: `.github/workflows/ci-runtime-products.yml:39-70,90-110,201-311`; source closure, native Linux-X64/Linux-ARM64/macOS-ARM64 builds, manifest/attestation/smoke test, immutable release.
- Setup source/generated pair: `.github-gen/sources/actions/setup-velnor-workflow/action.yml` -> `.github/actions/setup-velnor-workflow/action.yml`; both blob `4e48bc2694af7b3d1a969cb108234a9a92b515d3`. Config mapping `.github-gen/velnor-workflow.toml:281-283`; state output line 10.
- Report source/generated pair: `.github-gen/sources/actions/report-velnor-ci-outcomes/action.yml` -> `.github/actions/report-velnor-ci-outcomes/action.yml`; both blob `199083f950f67e8befd2282d8d525b105af761e0`. Config mapping lines 285-287; state output line 9. All five `ci-unit-*` workflows invoke it on both lanes.
- Candidate producer: `ci-unit-rust.yml:560-636`; only hosted `rust-velnor-workflow` callers pass `candidate_publish: true` (`ci-main.yml:1289`, `ci-pr.yml:1113`). Consumers: `ci-main.yml:221-235,271`, `ci-policy.yml:129-143,179`. Renderer emitters and negative tests are enumerated in JSON.
- Candidate consumer binds audited-tree closure and binary digest. `build_revision` is emitted but not deserialized/checked (`policy.rs:1245-1288,1541-1580`; s2 equivalent). Recorded as an actual contract boundary.
- Generated CI has no `--pin-build`; missing pinned renderer fails closed. JSON contains exact uppercase pin/build and escape-hatch maps.

## Recursive graph

- `ci-main.yml` -> all five `ci-unit-*` workflows: 35 reusable calls.
- `ci-pr.yml` -> all five `ci-unit-*` workflows: 35 reusable calls.
- `preview.yml:910` and `release.yml:4022` -> callable package signer.
- Roots: main push/dispatch; PR pull_request/dispatch; policy pull_request_target/dispatch; runtime-products main push/dispatch; maintenance closed-PR/schedule/dispatch; preview main path-filter/dispatch; release tag/dispatch; signer workflow_call only.
- Unit runners: hosted `ubuntu-24.04`; Velnor `[self-hosted, velnor-target-mvp]`; runtime-products includes hosted `macos-26`.
- Config declares 17 units (`.github-gen/velnor-workflow.toml:79-216`), providers/selectors at lines 17-20 and 38-44. Velnor is dispatch/trust gated; hosted is automatic.

## Actionlint / historical separation

Active config: `.github/actionlint.yaml:3-9`; invocations `ci-main.yml:274-283`, `ci-policy.yml:182-191`; renderer output/assertions in lib/s2 and primitive/config/runtime-product/scan/template-memory sources; tests `tests/synthetic_surface.rs:249-259`; tool pin `mise.toml:2,94-95`, `mise.lock:3-46`.

Historical/non-executable matches are separately classified in JSON: `plans/**`, `content/docs/**`, `migrations/**`, README, planning evidence. They do not contribute to the 82 old revision hits.

## Evidence limits

JSON has full exact path:line maps for old revision/closure/tag, runtime-v1, candidate-manifest, candidate_publish, build_revision, pin/build, setup/report, generator state, actionlint, recursive edges, and blob pins. Read-only Git inspection only; no build/test/dispatch/merge/review mutation; no GitHub live checks/reviews or external release bytes queried.

