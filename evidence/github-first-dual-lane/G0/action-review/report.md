# G3 action-independent review — preliminary dirty snapshot

## Review status

**Disposition: preliminary evidence only.** This report was written against a concurrently edited, uncommitted worktree. It is **not** an exact-candidate review and gives no final approval or rejection. Re-run the review against one identified detached source commit, a clean worktree, and the exact tests for that commit.

- Base input: `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`.
- Observed source worktree: `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-action-scanner`, branch `codex/github-first-action-scanner`.
- Observed state: source changes were uncommitted in `crates/velnor-workflow/src/s2/...`; author edits were concurrent. No source implementation was changed by this review.
- Scope: generic action metadata/scanning, action fixture realism, runner compatibility, and G3 action-independent acceptance design.
- Runtime/Docker: deliberately not operated. This is a G3-barrier no-operation, not evidence that the host or Docker is inaccessible.

## Pinned protocol references

The protocol source of truth is `actions/runner` at review checkout `/tmp/actions-runner-protocol-review` (commit `80bb1fb827...`; preserve the full checkout commit in the exact-candidate review). Relevant implementation anchors:

- Manifest filenames and precedence: `src/Runner.Common/Constants.cs:283-287`; `src/Runner.Worker/ActionManager.cs:725-739`.
- Metadata parsing and accepted `runs` keys: `src/Runner.Worker/ActionManifestManager.cs:363-435`.
- Runtime validation for `docker`, `node12`/`node16`/`node20`/`node24`, and `composite`: `src/Runner.Worker/ActionManifestManager.cs:441-518`.
- Input/default handling: `src/Runner.Worker/ActionManifestManager.cs:521-555`.
- Shell defaulting on Unix: `src/Runner.Worker/Handlers/ScriptHandler.cs:65-116`.
- Composite step condition and failure propagation: `src/Runner.Worker/Handlers/CompositeActionHandler.cs:382-451,469-474`.
- Composite nesting limit: `src/Runner.Worker/ActionManager.cs:181-187`.

These refs are design pins, not a claim that the dirty implementation matches them.

## Capability matrix observed in the dirty snapshot

| G3 requirement | Evidence in snapshot | Capability status | Required bounded shape |
| --- | --- | --- | --- |
| Enumerate real action roots | `crates/velnor-workflow/src/s2/scan/action.rs:52-99`; shared walk `crates/velnor-workflow/src/s2/file_walk.rs:26-44,110-149` | **Gap.** Filename matches are discovered globally outside `tests`/`benches`; `.github-gen/sources/**/action.yml` is tracked and is not excluded. `.github` and `dist` are excluded by the generic walk. | Use the shared `RepositoryShape`/typed-unit path contract. Default to root-owned action roots or an explicit action-root declaration. Do not create units from embedded YAML or generated `.github-gen` templates. Permit an action-local `dist` override only when the action root owns it. |
| Parse manifest variants | `action.rs:101-171`; runner parser refs above | **Partial.** Composite/Node/Docker branches exist, but current validation conflates host files with Docker container metadata and does not prove runner-equivalent fallback/precedence. | Match `action.yml` then `action.yaml` precedence; support bare `Dockerfile` fallback and `docker://`; distinguish host-local files from container `entrypoint`, `pre-entrypoint`, and `post-entrypoint`; validate required runtime fields exactly. |
| Composite `uses` / nested action semantics | `action.rs:101-145`; runner `CompositeActionHandler.cs:382-451,469-474` | **Gap.** Current code checks that `uses` is non-empty but does not resolve or execute nested local actions, conditions, shell defaults, or failure/cancellation propagation. | Record typed step metadata (`run`, `uses`, `with`, `if`, `shell`) and use an explicit fixture harness for nested local action references. Preserve condition and non-zero propagation semantics; do not flatten composite steps into an equivalent claim. |
| Composite script/path coverage | `action.rs:174-235,247-285` | **Gap.** Naive shell token inference misses direct paths after `&&` and cannot prove expression-based `${{ github.action_path }}` references. | Treat shell text as executable behavior, not a complete static path proof. Require explicit action-local reference checks plus a network-free executed consumer fixture. |
| JavaScript action coverage | `action.rs:101-171`; generic `file_walk.rs:110-149` | **Partial/failed observation.** `main`/`pre`/`post` fields are recognized, but action-owned `dist/index.js` is rejected by the generic `dist` exclusion. | Action-specific root walk must include owned `dist` and all declared JS entrypoints; preserve generic exclusions elsewhere. Test main/pre/post with a real consumer. |
| Docker action coverage | `action.rs:101-171` | **Partial.** Local path checks exist, but Docker image references and container-only entrypoint fields are treated as host file paths. Bare `Dockerfile` fallback is not proven. | Separate Docker build-context files from container command metadata. Test local Dockerfile, `docker://...`, bare Dockerfile, and pre/post entrypoints against runner source behavior. |
| Malformed/unknown runtime errors | `action.rs:161-171`; runner `ActionManifestManager.cs:441-518` | **Partial.** Unknown `using` errors exist; exact missing/unsupported field behavior is not proven by a real consumer. | Add table-driven malformed manifests: missing `runs`, missing required `using`, missing Node `main`, missing composite `steps`, missing Docker image/Dockerfile, and unknown runtime. |
| Real consumer and failure propagation | `crates/velnor-workflow/src/s2/primitives/github_action.rs:25-60,121-135` | **Gap.** Fixture code currently renders bash command strings; tests assert strings and do not execute a consumer or verify downstream stop behavior. | Add a network-free pinned action tree + consumer fixture. Execute with stub tools and marker logging; assert success, non-zero validator/build propagation, cancellation/skip behavior, and absence of downstream publish markers after failure. |
| Embedded/generated action exclusion | `file_walk.rs:110-149`; tracked files include `.github-gen/sources/actions/report-velnor-ci-outcomes/action.yml` and `setup-velnor-workflow/action.yml` | **Gap.** Current broad filename scan can turn generated templates into product units. `.github`/test fixture exclusion also prevents automatic discovery of real consumer fixtures. | Keep embedded YAML and generated source out of production unit discovery. Add explicit fixture registration for consumer tests instead of weakening the product walker. |
| Shared scanner architecture | `crates/velnor-workflow/src/s2/scan/mod.rs:37-78`; `s2/mod.rs:537-595`; `primitives/mod.rs:574-627` | **Partial.** A `GithubAction` unit/primitive was added in the dirty snapshot, but the scanner, registry, pipeline, shape, and watch contracts need one shared typed path. | Extend the existing scanner/primitive registry and shape/digest/watch model. No parallel action-only walker or ad hoc fixture schema. |

## Real-fixture evidence to preserve

The existing action-role fixture is a useful consumer target, but it must be explicitly registered rather than discovered by the product walker:

- `/tmp/velnor-g3-action-roles.ueumsS/role-action/action.yml:10-75` is a composite action with `github.action_path` script references, `hadolint`, `jackin-role validate`, Buildx setup/build, and inputs.
- `/tmp/velnor-g3-action-roles.ueumsS/jackin/crates/jackin/tests/fixtures/roles/jackin-sentinel/.github/workflows/validate.yml:1-16` is a real consumer shape, but currently uses mutable `jackin-project/jackin-role-action@main`; an acceptance fixture must pin an immutable SHA.
- Existing executable harness patterns are in `crates/velnor-workflow/src/consumer_negatives.rs:544-609,739-780,976-1013`: temporary fixtures, stub tools, PATH injection, non-zero assertions, and downstream-marker absence. Reuse this architecture for action fixtures.

## Required future design

1. **Bound discovery.** Add action-root declarations/typed units through the existing repository-shape path. Exclude `.github-gen`, embedded templates, and generic non-action paths. Include `dist` only beneath an owned action root.
2. **Match runner metadata.** Implement only behavior pinned to the runner refs above: yml/yaml precedence, runtime-specific required fields, Docker image/Dockerfile distinction, Node entrypoints, composite step metadata, and runner shell defaults. Do not invent a mandatory shell field.
3. **Separate static and executable proof.** Static scanning proves manifest shape and declared local files. A fixture consumer proves action behavior, nested references, condition handling, command failure propagation, and cancellation/skip outcomes. A token parser must not be treated as complete shell analysis.
4. **Use one fixture contract.** Register a pinned action tree and consumer through the scanner’s existing typed fixture/shape mechanism. Stub network/tool boundaries (`gh`, artifact download, `curl`, archive tools, `jackin-role`, hadolint, Docker/Buildx) and log ordered markers. Avoid mutable refs and live network.
5. **Keep role/runtime work separate.** Generic action scanning cannot by itself close role manifest/image-runtime or multi-arch publisher obligations. Those remain explicit G3/G4 work items.

## Acceptance tests for the exact candidate

Run only after the implementation is committed/detached and the worktree is clean:

- Static matrix: composite with `run` + local `uses`, expression-based `github.action_path` script, JS `main`/`pre`/`post` under `dist`, local Dockerfile, bare Dockerfile fallback, `docker://` image, and malformed/unknown runtime cases.
- Discovery matrix: root action, explicit nested action, `.github` consumer fixture, generated `.github-gen` action, embedded YAML, generic `dist`, and unrelated `action.yml` under tests/benchmarks. Assert only intended typed units are emitted.
- Consumer success: immutable-SHA action reference executes the download/validate/build path and emits the expected terminal marker.
- Failure propagation: each validator/build stub exits non-zero; assert the action/consumer fails and no downstream publish marker is emitted.
- Composite behavior: conditional step skipped/executed according to input; nested local action failure is retained; cancellation/failed result is not converted to success.
- Tooling: `cargo check -p velnor-workflow`; focused action scanner tests; full relevant package tests; `git diff --check`; exact fixture execution. Record commands and commit SHA in the final report.

## Dirty-snapshot observations (not gate results)

Previously run against the mutable snapshot, not to be rerun here:

- `cargo check -p velnor-workflow`: passed.
- `cargo test -p velnor-workflow scan::action -- --nocapture`: failed 2 tests (2 passed, 2 failed): direct script after `&&` was missed; action-owned `dist/index.js` was rejected by the shared `dist` exclusion. The log also recorded source modification during compilation.

These observations identify design gaps only. They do not constitute exact-candidate test results or an approval/rejection.

## Review stop condition

No final disposition until the author supplies one exact source commit, clean tree evidence, runner checkout commit, and the acceptance tests above. This report intentionally records the G3 barrier and remains read-only with respect to source/runtime/Docker.
