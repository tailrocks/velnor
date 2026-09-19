# Protected validator bootstrap transition: timing and identity preflight

Status: planning evidence only; no authority write, merge, release, dispatch,
ruleset mutation, or production source edit.

Observation: 2026-09-20. This audit refreshes origin/main after the prior
admission probe and challenges the proposed protected pre-main transition. It
uses the supplied SPEC current macOS rule: the verified arm64 mapping is macOS
27 with exact label xcode-27; macos-26, macos-15, and aliases are not acceptable
fallbacks.

## Refreshed main and old-checker result

| Item | Exact value |
| --- | --- |
| Refreshed origin/main | d20d4d1d17590cca85b501d982cbaad70d42c641 |
| Parent | 1048337062ea625fada1b4f7c07f2feed75f60c7 |
| Commit | fix(workflow): route hosted Apple jobs to macOS 26 |
| Fixture | /private/tmp/velnor-current-d20-admission-fixture |
| Fixture tree | ec2d82aa83a1b2db692d1d3d46d48568d756c602 |
| Fixture fingerprint | 9e9bdecae910121647ef984865d930fbd51e0da0f4161291c60410171288d254 |
| Trusted checker | 0dc79895ff1c5e88be7c3822c437e1c5b5282e12 |
| Checker binary SHA-256 | ade826fd3a27de43cfce55acfda870493b4921770199fe0afaa653b9a96f1750 |

The fixture was a detached read-only worktree at exact origin/main. The raw
check was not normalized with --force:

~~~sh
OLD=/private/tmp/velnor-old-target/release/velnor-workflow
ROOT=/private/tmp/velnor-current-d20-admission-fixture
(cd "$ROOT" && "$OLD" . --plain --check)
~~~

Result: exit 1; generated files differ at .github/actionlint.yaml,
.github/ci/.github-actions-generator-state, and
.github/workflows/ci-runtime-products.yml.

The semantic invocation used an empty candidate manifest and no --pin-build:

~~~sh
VELNOR_WORKFLOW_PINNED_BINARY="$OLD" "$OLD" policy \
  --workflow-root "$ROOT" \
  --head-sha d20d4d1d17590cca85b501d982cbaad70d42c641 \
  --base-sha 1048337062ea625fada1b4f7c07f2feed75f60c7 \
  --base-revision 0dc79895ff1c5e88be7c3822c437e1c5b5282e12 \
  --ruleset-contexts DCO,Policy,ci-required \
  --candidate-manifest ""
~~~

Result: exit 1, generated-tree only; 10/11 rules pass. The raw failure is the
current-main fact. Any plan that uses old --force first must record the
normalization separately and may not claim the unchanged tree was admitted.

The refreshed workflow has macos-26 at .github/workflows/ci-runtime-products.yml
lines 104-107. Old 0dc accepts the broad macos-* prefix, so this semantic result
does not prove the SPEC newest-macOS obligation. Current main is stale in two
independent ways: old-renderer drift and a forbidden lagging hosted-Mac label.

## Measured first-main scheduling

Read-only GitHub API snapshot for d20d4d1d:

| Workflow | Run | Trigger/start | Result at observation |
| --- | ---: | --- | --- |
| Runtime products | 35475920678 | push, created/started 23:21:55Z | success 23:25:24Z |
| CI / main | 35475920826 | push, created/started 23:21:55Z | in progress; Policy entered old candidate acquisition at 23:22:56Z |
| Preview | 35475920808 | push, created/started 23:21:55Z | failed 23:22:16Z; identity's old policy failed |

The runtime job graph proves no publisher-before-main ordering:

| Job | Runner label | Start | Result |
| --- | --- | --- | --- |
| Resolve runtime closure | ubuntu-24.04 | 23:22:03Z | success |
| Build runtime (Linux-ARM64) | ubuntu-24.04-arm | 23:22:16Z | success |
| Build runtime (Linux-X64) | ubuntu-24.04 | 23:22:17Z | success |
| Build runtime (macOS-ARM64) | macos-26 | 23:22:25Z | success |
| Publish runtime products | ubuntu-24.04 | 23:25:07Z | success |

All three push workflows were scheduled in the same second. The Mac cell began
before runtime publication finished, and before CI/main Policy completed. The
workflow run API reported the Mac job labels as macos-26; this directly proves
that publishing B after the push cannot repair first-main obligations.

The exact static trigger graph at this main revision is:

~~~text
push refs/heads/main
├── .github/workflows/ci-main.yml
│   ├── plan (ubuntu-24.04; old runtime setup)
│   ├── policy (ubuntu-24.04; old 0dc setup + candidate acquisition)
│   ├── generated unit jobs after plan+policy
│   └── ci-required (always; aggregates plan/policy/unit jobs)
├── .github/workflows/ci-runtime-products.yml
│   ├── closure (ubuntu-24.04)
│   ├── build Linux-X64
│   ├── build Linux-ARM64
│   ├── build macOS-ARM64 on macos-26 [SPEC negative]
│   └── publish
└── .github/workflows/preview.yml
    ├── identity (ubuntu-24.04; old 0dc policy)
    └── metadata/deb/sign/publish only after identity
~~~

ci-policy.yml is not a push-main workflow; it triggers on pull_request_target and
manual dispatch. release.yml is tag-triggered, not push-main. Therefore a
handoff that updates only the pull_request_target path leaves first-main
ci-main.policy and preview.identity on old 0dc logic. The current source has
policy invocations in ci-main.yml, ci-policy.yml, preview.yml, and release.yml;
the first-main transition must account for every applicable policy invocation,
while leaving application-runtime consumers separate from validator B.

The current protect-main ruleset was queried read-only:

| Field | Value |
| --- | --- |
| Ruleset | 19573071 / protect-main |
| Updated | 2026-09-16T08:47:23.561+07:00 |
| Required contexts | DCO, ci-required, Policy |
| Strict required-check policy | false |
| Required approving reviews | 0 |
| Allowed merge methods | merge, squash, rebase |
| API response SHA-256 | eb3d84d532e97b5dc64772af83a2e53f85427a824086b2bd6d8fa24752e2e46b |

The required checks have no visible integration binding in the current ruleset.
A context named Policy is not proof of a particular B-backed workflow without an
exact run/check identity check or protected ruleset integration binding.

## Exact preflight plan

The following assertions are gates. Any false assertion aborts the transition;
no fallback or wrapper turns it green.

### 1. Freeze resulting source before B exists

The authority must capture and sign a transaction record containing repository
ID/name, current protected main tip and expected parent, PR number/head/base,
merge method, final target commit SHA, final target tree SHA, complete closure
listing, SHA-256/blob IDs for validator/policy workflows, setup action, generator
config/state, runtime workflow, B publisher workflow, expected B closure, product
tag, release tag, and manifest digest.

Before build and immediately before ref update, assert:

~~~text
git rev-parse <target-sha>^{commit} == expected-target-sha
git rev-parse <target-sha>^{tree}   == expected-target-tree
git rev-parse refs/heads/main       == expected-parent (before CAS update)
~~~

Reject a PR SHA, staging ref, merge message, or commit with a different parent
or tree. Merge, squash, and rebase can produce different final SHAs. The
authority must either create the exact immutable target commit itself and
CAS-update refs/heads/main, or prove an equivalent atomic primitive. Building
from a PR/staging commit and later allowing GitHub to synthesize another merge
commit is a source-identity failure.

### 2. Old-schema admission without hiding drift

For source admission:

* .github-gen/velnor-workflow.toml contains no [policy.validator]; old
  RepoGenerationConfig/PolicySection strict parsing remains unchanged.
* The only typed transport is an old-supported [[static_files]] mapping from a
  repository-relative sidecar source to a destination under .github/.
* Run actual 0dc --plain --check and 0dc policy against the exact fixture with
  --candidate-manifest ""; record both exit codes and all findings.
* Never run old --force in proof. If normalization is a separate experiment,
  retain raw failure and normalized success as distinct records.
* Reject sidecars not reproduced by old render, or containing B pin, xcode-27,
  fallback, candidate authority, or self-attested authority.

This proves compatibility only, not B or transition authorization.

### 3. Build and bind B before main can move

The pre-main actor must build exactly one separate product:

~~~text
schema:    velnor.workflow-policy-validator.v1
product:   velnor-workflow-policy-validator
purpose:   policy-validator
platform:  Linux-X64
runner:    ubuntu-24.04
profile:   release
features:  ""
~~~

Before release publication assert:

* clean detached checkout at frozen target SHA; HEAD, target tree, source
  closure, and binary --revision/--closure agree;
* locked release build with isolated Cargo state; no candidate, local binary,
  --pin-build, application runtime product, ARM asset, or Mac job;
* exact publisher workflow path/blob, repository, intended refs/heads/main,
  target SHA, run/attempt, numeric job/check-run, successful build/upload steps,
  artifact ID/name, service-ZIP digest, and inner payload digest;
* signed custom binding and standard binary provenance verify against the exact
  protected publisher workflow and source identity;
* product ID/schema/purpose/asset/tag/closure/platform/profile/features/source
  SHA/release target/binary digest-size-mode/namespace all match;
* existing release/tag is absent or an exact immutable match. Same closure with
  a different source SHA or target is a hard failure.

The release must be fully published and independently verified before main ref
update. Draft-only is not a consumer product. First-main cannot wait for a B
publisher triggered by the same push.

### 4. Prove source-ref claim, or stop

The proposed manifest says source.ref=refs/heads/main and exact final main SHA.
A normal GitHub Actions run cannot truthfully make that claim before final main
ref exists. A staging branch, PR head, or detached future commit must not be
relabeled as main provenance.

The protected authority needs one demonstrable primitive:

1. owner-controlled creation of the exact target commit object, build/release
   and signed custom binding against that immutable object, then compare-and-
   swap refs/heads/main to the same SHA; or
2. an independently verified platform transaction guaranteeing the exact future
   target SHA and source-ref provenance before any main push workflow starts.

If the platform cannot attest a future refs/heads/main ref before update, use a
custom owner-signed transaction predicate and make consumers require it;
ordinary SLSA source-ref verification alone is insufficient. A refs/heads/main
claim without this mechanism remains an unresolved source-identity gap.

### 5. Handoff every policy path before merge

Publishing B alone changes no base workflow. Before merge, the protected handoff
must prove the exact bootstrap PR tree with B-backed policy and retain DCO,
ci-required, and all native obligations. It must address:

* base pull_request_target ci-policy.yml (old 0dc plus candidate);
* push-main ci-main.yml Policy job (old 0dc plus candidate);
* push-main preview.yml identity policy step (old 0dc);
* any other policy invocation selected by first-main.

The external check must not merely be named Policy. Verify check/run workflow
path, workflow source SHA/blob, event, repository, PR number/head/base, target
tree, actor/integration, run attempt, job ID, and terminal conclusion. Require
actual B product identity in evidence. A failed old child must not be hidden by a
green wrapper; if old Policy is temporarily removed from required contexts, the
replacement context and scope must be explicitly protected and independently
reviewed.

Current ruleset strict=false, zero required reviews, and no visible integration
binding are insufficient. Freeze final target, require exact B-backed check, and
reject stale or same-name checks from another workflow.

### 6. CAS merge, then assert first-main invariants

Only after B is published, attested, and B-backed handoff is terminal success may
the authority update main. CAS must fail if main moved. Immediately afterward:

* B release exists, immutable, target-bound, published, independently verified
  before any push-main job starts;
* every push-main run has exact head_sha=target_sha, branch main, event push,
  expected workflow path/source, and allowed run attempt;
* ci-main Policy and preview policy consumers use B, not old 0dc, candidate
  manifests, or local fallback;
* runtime application matrix retains Linux-X64, Linux-ARM64, and macOS-ARM64
  on exact xcode-27; reject macos-26, macos-15, macos-latest, empty matrix,
  if:false, skipped native jobs, or silent reroute;
* application runtime publisher remains separate. B is Linux-X64
  policy-validator only.

Do not accept green first-main if native jobs skip or runtime succeeds with a
forbidden Mac label. Current d20's successful macos-26 run is a concrete
negative fixture.

### 7. Cleanup and re-prove

After one verified B-backed PR and one verified main run, restore ordinary
required contexts, remove temporary authority/sidecar/fallback permissions and
workflows, and rerun typed generation plus raw policy checks. Evidence must show
no bootstrap references remain.

## Timing and source-identity challenge matrix

| Proposed claim | Concrete failure mode | Required fail-closed assertion |
| --- | --- | --- |
| Publish B after merge | Push-main runtime starts Mac before publisher; d20 Mac began 23:22:25Z, publisher only 23:25:07Z | B final release verified before CAS main update |
| Build from PR, label source main | PR/staging SHA differs from resulting merge/squash/rebase SHA | target commit/tree and source ref match exact main SHA |
| GitHub merge preserves planned SHA | merge metadata/parent/tip can change; current ruleset permits all methods | owner-controlled exact commit/CAS or equivalent atomic primitive |
| Future main ref receives ordinary SLSA provenance | ref does not exist before pre-main build; source-ref claim is not truthful | custom owner-bound transaction predicate or proven primitive |
| B publication changes Policy | base pull_request_target, push-main ci-main, and Preview still point to old 0dc | inspect every policy invocation; prove B-backed check before merge |
| Context Policy is trusted | current ruleset has context only, strict=false, no visible integration binding | exact workflow/run/check identity and protected integration binding |
| Old checker passed after regeneration | --force hides raw current-main drift | raw check mandatory; force result separate |
| macos-26 is close enough | SPEC requires macOS 27/xcode-27; d20 proves old label executes | exact label/host/arch assertion; reject older/alias labels |
| Draft release is available | consumer cannot use unpublished draft as B | draft=false, assets/attestations/API checks complete before main |
| Same closure means same product | closure can match while source SHA/target/product differs | exact source SHA, target, manifest, asset, namespace equality |
| Artifact name/run ID proves producer | REST metadata omits full job/step binding; stale/duplicate artifact possible | signed binding plus live run/attempt/job/check/step/artifact cross-check |
| External workflow in PR can bootstrap itself | base pull_request_target executes base workflow before merge | protected pre-main actor already exists outside candidate semantics |
| Skipped old Mac is temporary | native work is missing, not replaced | complete xcode-27 matrix remains required |

## Conclusion

The measured sidecar contract remains the only old-0dc source-admission path:
strict old config plus existing static_files, with raw --check and empty-candidate
semantic proof. It is not authority.

Refreshed main proves the ordering hazard in live execution and adds a hard
negative: macos-26 ran successfully but violates the newest-Mac rule. The
protected transition is conditionally viable only with actual pre-main
source-bound B publication, exact old-to-B handoff across every policy consumer,
atomic target-SHA/main update, and fail-closed required-check identity. Those
guarantees are absent from current repository/ruleset. No approval is issued.

