# GitHub Action consumer-contract rereview — exact `b52d250b`

## Disposition

**Changes required. Bounded source-only review; no merge, rollout, macOS
payload, Docker operation, or host runtime.**

Reviewed exact revision
`b52d250b4d509c63291d4c857f668aadd5a5d86d` on
`codex/github-first-action-scanner` from clean detached checkout
`/private/tmp/velnor-action-review-b52`.

Runner comparison uses `actions/runner`
`80bb1fb827fa44d489263061e71ef4adba7ad8cd` at
`/tmp/actions-runner-protocol-review`.

## Findings

1. **P1 — the generated consumer workflow cannot execute its local action on a
   fresh GitHub-hosted job.**
   `crates/velnor-workflow/tests/fixtures/github-action-consumer/workflow.yml:5-10`
   starts with `uses: __VELNOR_ACTION_PATH__` and has no
   `actions/checkout` step. After marker replacement this is `uses: ./`, but
   the runner workspace is not populated implicitly; nested local actions and
   `action.yml` are therefore absent. Require a pinned checkout step before
   the local action, and test the rendered workflow for it.

2. **P1 — the generated graph is emitted but not part of automatic CI, and its
   source is not in the unit watch set.** The fixture declares only
   `on: workflow_dispatch` (`workflow.yml:1-3`), while
   `GithubActionFixtures::render` watches only `success_fixtures`,
   `failure_fixtures`, and `skip_build_fixtures`
   (`crates/velnor-workflow/src/s2/primitives/github_action.rs:53-68`), not
   `consumer_workflows`. A checked-in consumer change can therefore neither
   trigger the action unit nor run the generated workflow in PR/full CI.
   Add the fixture to `watch` and wire the emitted workflow into the reviewed
   PR/full path (or an explicitly invoked reusable workflow), retaining a
   manual trigger only if intentional.

3. **P1 — generated workflow assertions do not prove the requested build and
   failure contract.** `workflow.yml:22-51` covers only downloader failure and
   checks skip-build only through `steps.action.outputs.result` plus a
   non-empty marker. It never asserts that `hadolint`/`buildx` are absent, and
   it has no generated consumer cases for validator, lint, Buildx, or
   downstream failure. The in-process test matrix at
   `crates/velnor-workflow/src/s2/primitives/github_action.rs:2077-2342`
   does cover those cases, but it is test-only and does not execute the
   generated workflow. Add generated workflow assertions/cases for no-build,
   each non-zero stage, output propagation, and downstream suppression.

4. **P1 — scanner/runtime disagree on uppercase Docker schemes.** The scanner
   accepts case-insensitive `DOCKER://` (`crates/velnor-workflow/src/s2/scan/action.rs:1523-1528`)
   and explicitly treats it as a valid remote image
   (`:3150-3217`), matching runner `ActionManager.cs`'s ordinal-ignore-case
   scheme check (`:1467`). Velnor runtime then uses case-sensitive
   `image.strip_prefix("docker://")`
   (`crates/velnor-runner/src/action.rs:780-796`) and misclassifies
   `DOCKER://...` as a Dockerfile path. Normalize the scheme once in the
   runner-owned parser or reject uppercase consistently; do not leave scanner
   acceptance and runtime execution divergent.

5. **P2 — scanner image validation is weaker than the runner-owned image
   contract.** `is_docker_image_reference` only checks a non-empty
   `docker://` suffix (`action.rs:1523-1528`), so malformed values such as
   `docker://--privileged` pass scan. Runner action execution validates the
   stripped value with `ImageReference::parse` and rejects these values
   (`crates/velnor-runner/src/action.rs:781-793`; parser grammar at
   `crates/velnor-runner/src/docker_argv.rs:81-128`). Reuse the typed parser
   for scanner validation or make the scanner reject the same grammar.

## Closed prior findings

- Bare plain Docker image and missing composite `runs.steps` are fixed and
  tested: `action.rs:1212-1215,1455-1475,2456-2489,2805-2895`.
- YAML scalar/collection/null/empty/duplicate-key preflight remains covered by
  the scanner tests (`action.rs:2064-2454`).
- Traversal/empty local and external `uses` paths remain fail-closed; explicit
  plugin runtime rejection remains present.
- The generated consumer surface is now structurally emitted through
  `Surface.files` and `generated_files_with_surface`
  (`s2/primitives/mod.rs:669-680,698-840`; `s2/mod.rs:5591-5626`), but the
  execution/trigger/assertion findings above remain open.

## Verification

Passed on the clean exact checkout:

- `rtk cargo test -p velnor-workflow --locked s2::scan::action::tests -- --nocapture` — **25 passed** (`19` suites; `1872` filtered).
- `rtk cargo test -p velnor-workflow --locked s2::primitives::github_action::tests -- --nocapture` — **12 passed** (`19` suites; `1885` filtered).
- `rtk cargo test -p velnor-runner --all-features --locked action_contract::tests -- --nocapture` — **7 passed** (`18` suites; `2517` filtered).
- `rtk cargo test -p velnor-workflow --locked` — **1897 passed** (`20` suites; `59.85s`).

No source files were edited. Detached review checkout stayed clean at the
exact revision.
