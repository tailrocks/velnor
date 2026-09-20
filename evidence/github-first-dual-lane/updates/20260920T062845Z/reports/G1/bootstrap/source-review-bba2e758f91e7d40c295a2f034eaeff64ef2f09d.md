# Independent source review — `bba2e758f91e7d40c295a2f034eaeff64ef2f09d`

**Bounded verdict: CHANGES REQUIRED.** The successor materially improves the workflow contract: full `.github` archiving, recursive reachable local-action files, raw bytes/mode comparison, full workflow-line normalization, and API/tree-bound contract digests. Two reachable local-action/source-closure gaps remain. Exact policy tests and clippy are also red. Generated drift is not waived; this is not a G1 approval.

## Frozen provenance

- Reviewed commit: `bba2e758f91e7d40c295a2f034eaeff64ef2f09d`
- Parent: `643b304a9fd011e016d32efa963fc3edfa8b2a7e`
- Branch/worktree: `codex/g1-bootstrap-isolation`, `/private/tmp/velnor-g1-bootstrap`
- Worktree was clean and matched `origin/codex/g1-bootstrap-isolation` at review.
- `643b..bba2` changes only `src/s2/mod.rs`, `src/s2/policy/tests.rs`, and the transport test fixture; no generated workflow changed.
- No source, generated workflow, GitHub, publication, authority, or installation state was changed. This report is the only external write.

## Positive structural changes verified

The exact scanner now:

- archives the tracked `.github` tree rather than only `.github/workflows` (`src/s2/mod.rs:4654-4658`);
- rejects non-regular `.github` members and records member mode plus raw bytes (`:4709-4721`);
- resolves recursive local reusable-workflow and `.github/actions` edges (`:4726-4741,4788-4827`);
- compares complete normalized workflow text for every reachable node (`:4829-4880`);
- compares the reachable local-action file set, bytes, and modes (`:4849-4892`); and
- emits a base contract digest plus a binding over base commit, Git tree, API tree digest, and contract (`:4894-4912`), then transports and rechecks both values in the fresh verifier (`:6008-6015`).

Safe execution of the exact embedded Python scanner against temporary archives confirmed rejection of prior root-workflow cases: Python artifact-service POST, opaque pinned action, top-level permissions change, job container/service change, action-input change, local called-workflow drift, and reachable `.github/actions` file-byte/mode drift.

## Blocking finding 1 — local action resolver excludes valid local action paths

**P1 — `src/s2/mod.rs:4682-4683,4734-4741,4788-4812`.** Local actions are recognized only by:

```python
local_action_pattern = re.compile(r"^\./(.github/actions/[^ #]+)")
```

All other `./...` action references are exempt from the pinned-action check but are not resolved into the source closure. GitHub local actions can be referenced from other repository-relative directories. A base workflow with an existing `uses: ./actions/setup` is therefore admitted, while a head changes `actions/setup/action.yml`; the exact scanner accepted the base/head pair (`rc=0`). The file is also outside the `.github` archive supplied to the scanner.

This is a reachable publisher-capability bypass whenever the existing local action can upload or invoke a publisher. Fix by resolving every repository-relative local action path, canonicalizing it safely, and recursively binding its manifest and implementation; or reject every local path outside the explicitly reviewed canonical directory. Do not exempt arbitrary `./` values from action validation.

## Blocking finding 2 — action closure is not the runtime dependency closure

**P1 — `src/s2/mod.rs:4849-4859,4882-4892`.** The new byte/mode comparison includes only files beneath the local action directory root. It does not follow files executed or read by a composite/JavaScript/Docker action, workflow `run:` commands, or action inputs. A reachable local action can keep its `action.yml` unchanged while running a repository-relative script outside its directory.

Exact repro: both workflow archives contained an unchanged reachable action manifest:

```yaml
runs:
  using: composite
  steps:
    - shell: bash
      run: bash .github/scripts/publish.sh
```

Only `.github/scripts/publish.sh` changed between base and head. The scanner accepted (`rc=0`) because `reachable_action_files()` filters to `.github/actions/setup/**`; the changed script is present in the `.github` archive but is omitted from the reachable action closure. The same class exists for `run: ./scripts/publish.sh` in a reachable workflow and for an action input pointing at a changed repository file.

The fix must either compute a complete trusted execution/source closure for every reachable workflow/action (including referenced scripts, manifests, Dockerfiles, and input-selected files), or constrain the contract so reachable publisher paths cannot execute repository-controlled files outside the measured closure. Comparing YAML and one action directory is not full workflow semantics.

## Unrelated workflow identity scope — conditional pass, not authority proof

An added unrelated `.github/workflows/evil.yml` containing either a Python artifact-service POST or a pinned opaque action was accepted by the scanner (`rc=0`). This is intentionally scoped: only the reachable `.github/workflows/ci-pr.yml` graph may mint the candidate namespace contract. The acquire API filters separately bind the numeric `ci-pr.yml` workflow ID, pull-request run ID/attempt, exact head SHA/repository IDs, exact `candidate_producer` job, and artifact `workflow_run.id` (`src/s2/mod.rs:5133-5175`). A separate workflow run cannot satisfy that artifact selection.

This is safe only if those API/run identity checks remain authoritative and a called workflow is reachable from `ci-pr.yml`; the scanner correctly rejects an unpinned/external action once its workflow is reachable. No hosted or real API canary was run, so this remains conditional evidence, not a G1 claim.

## Base/source/tree binding — source wiring present, scanner inputs remain caller-trusted

The production acquire shell verifies target repository identity, obtains both commit/tree objects and recursive API tree digests, fetches both commits from the same target repository, and checks Git commit tree IDs equal the API tree IDs before invoking the scanner (`src/s2/mod.rs:5048-5102`). The fresh verifier repeats those checks from a new bare source repository (`:5997-6007`). The scanner binds the normalized base contract and local-action bytes/modes to the supplied base SHA/tree/API digest, and the verifier compares the recomputed two-value output to the handoff (`:4894-4912,6012-6015`). This closes the prior “compare two unbound archives” path when the generated caller is the authority.

The Python scanner itself only validates identity argument shape. A standalone synthetic invocation can pair arbitrary archive bytes with well-formed fake SHA/tree/API arguments and receive a contract digest; that is not a production bypass while the surrounding Git/API checks remain unchanged. Likewise, a malicious base commit remains accepted as the contract by design: the binding proves which target-repository commit/tree supplied it, not that arbitrary base source is safe. Protected-base/ref trust and the real API/object authority remain external gates and were not executed here.

## Exact test state

- `rtk cargo test --locked -p velnor-workflow --lib s2::policy -- --nocapture`: **25 passed, 1 failed**. Failure: `candidate_transport_rejects_malformed_manifest_and_open_publishers` still requires the removed scanner marker `shell step can POST to the Actions artifact service` (`src/s2/policy/tests.rs:638`).
- `rtk cargo test --locked -p velnor-workflow --test bootstrap_transport -- --test-threads=1`: **4 passed**.
- `rtk cargo clippy --locked -p velnor-workflow --all-targets --all-features -- -D warnings`: **failed** at `src/s2/policy/tests.rs:1327`; `assert_candidate_transport_acquisition` is `103/100` lines (`clippy::too_many_lines`).
- `rtk cargo fmt --all -- --check`: pass.
- `rtk git diff --check 643b304a9fd011e016d32efa963fc3edfa8b2a7e..bba2e758f91e7d40c295a2f034eaeff64ef2f09d`: pass.
- Focused generated fixed-point test: **0 passed, 1 failed**, `ci-policy.yml drifted from the generator` at `src/s2/mod.rs:17243`; 1700 tests filtered. No generated output was regenerated or waived.

No hostile binary, Docker, Mac runtime, hosted canary, dispatch, real GitHub API, authority mutation, or publication was executed. Resolve both source-closure findings and the test/clippy regressions, regenerate outputs, then re-freeze before any G1 decision.
