# Generator activation and CI artifact architecture

Status: active and incomplete. This is the authoritative execution record for
the generator activation, policy validation, and CI artifact dependency work.
Evidence was refreshed on 2026-09-22 against `origin/main` at
`4dec6b9ec28b0d51cb370fd8f5d5401c6186adf0`; the local integration checkout is
`b3b3c3cf38f9c71cf5dae75a2694bd2eb1bd31a0`; integration commits are listed
below.

## Structural causes

The two incidents are one architectural failure class: a consumer was allowed
to treat an untrusted or nonexistent producer as the authority for active
workflow output.

- Preview accepted `TreeComparison::Candidate` whenever `head == base` was
  false. That inferred “not mainline” from commit equality instead of carrying
  an explicit event and trust context. Candidate-rendered output could pass a
  PR check while its declared active pin still rendered a different tree.
- Main Policy searched for a `ci-pr.yml` `pull_request` run at the main head
  SHA. A squash/push SHA is not a PR-head producer identity. The consumer did
  not model producer existence or terminal failure, so it reserved a runner
  for a 900-second guessed-producer polling loop.
- The prior promotion contract proved local render/stamp atomicity but did not
  prove that every activated runtime product had been published, complete,
  retrievable, and compatible before changing the active reference.

These structures also permit stale event identity, false provenance,
partial publication, and invalid activation on other supported platforms.

## Decision and invariants

Use one active-tree invariant for PR, prospective integration, main, Preview,
and release consumers:

> Active generated output is the deterministic render of the declared,
> authenticated active renderer over the audited configuration and complete
> scan/static inputs.

The lifecycle is qualify → publish → activate:

1. Candidate source is qualified in a restricted ephemeral lane. Its output is
   disposable test data and cannot choose or replace the trusted validator.
2. A trusted controlled publisher builds and publishes the immutable runtime
   for every supported platform. Publication must produce a complete,
   durable, provenance-checked readiness manifest before activation.
3. One protected increment activates the exact renderer reference, compatible
   configuration, complete generated tree, and ownership metadata. Promotion
   checks the manifest's schema, closure, source revision, platform set,
   digest shape, expiry, revocation state, and exact renderer identity before
   mutating the tree.

Event context, source/build identity, binary digest, generated-tree identity,
policy authority, audited tree, and run/attempt identity remain separate typed
values. Artifact consumers use explicit same-run edges for candidate work or an
authenticated exact immutable product reference for activated runtimes. A
missing producer, terminal producer failure, incompatible platform, invalid
provenance, or expired product is terminal; it is never converted into a
runner polling loop.

## Existing work disposition

| Work | State and disposition |
| --- | --- |
| Incident recovery pin bump in #1038 | Merged and CI-verified; recovery only, not architecture proof. |
| Existing local `promote` transaction | Preserved; publication-readiness validation is being added at its existing gate. No duplicate promotion mechanism. |
| `ci-runtime-products.yml` | Reused as an existing publication surface for investigation; its current lifecycle is not yet accepted as complete activation proof. |
| #978, #979, #980 drafts | Inspected; overlapping or unresolved bootstrap/prospective-main designs. None is treated as an implementation or merge authority. |
| Candidate-rendered PR exception and guessed `ci-pr.yml` acquisition | Superseded in the active source paths; generated consumers still require regeneration and hosted proof. |

## Evidence and timing

| Observation | Evidence | Result |
| --- | --- | --- |
| Preview incident | [run 35623133972, job 106410846252](https://github.com/tailrocks/velnor/actions/runs/35623133972/job/106410846252) | Generated-tree check failed in about 17–18s because candidate output differed from the active pin. |
| Main incident | [run 35623133871, job 106410848097](https://github.com/tailrocks/velnor/actions/runs/35623133871/job/106410848097) | Policy failed after about 920s; about 906s was absent-producer acquisition. |
| Recovery | Preview job `106420138590` and main Policy job `106420140784` | Passed in about 23s and 35s; these prove recovery, not complete lifecycle correctness. |
| Current main baseline | CI Main run `35629948234` | About 18m30s wall; Docker job about 788s and runtime publication about 597s. This remains a performance violation against the 120s requirement. |
| Foundation PR Policy | [run 35641013363](https://github.com/tailrocks/velnor/actions/runs/35641013363) | Passed in 29s at `cf2e236`; the active renderer validated the active tree without candidate acquisition. |
| Cache-proof PR Policy | [run 35643201030](https://github.com/tailrocks/velnor/actions/runs/35643201030) | Passed in 25s at `04c520d8`. The checked-in workflow is still rendered by the old active pin and executed its legacy candidate-acquisition step, so this is recovery evidence only. |
| Foundation final-SHA CI | [run 35641107449](https://github.com/tailrocks/velnor/actions/runs/35641107449) | Cancelled after two jobs were stuck far beyond 120s: `Set up Mr. Boxington` and `Clippy check`. This is retained as a hosted performance failure, not a green verification. |

The detailed timing observations remain in
[`plans/ci-performance/README.md`](ci-performance/README.md) and its retained
raw observations. No staged promotion or green recovery run is counted as a
speedup.

## Implemented integration commits

- Both top-level and schema-2 policy validators reject candidate-rendered
  output as the active tree; the old semantic exception is removed.
- The generated policy path no longer discovers a candidate through a
  head-SHA PR-run search or waits 900 seconds for a guessed producer.
- The existing promotion command now requires a publication-readiness
  manifest and validates it before local render/stamp mutation.
- `4ad4b28`, `a111a04`, `11342007`, `cf2e236`, and `30a7995` are pushed on
  PR #1044. The active renderer regenerated the ownership state at `11342007`.
- Transportable same-run prerequisites now require producer `success`; a
  skipped, failed, or cancelled producer cannot unlock a consumer-side rebuild.
- The setup action now refreshes and attests the immutable release manifest on
  every invocation, including a cache hit; cached executable bytes are then
  checked against that freshly authenticated manifest. This removes the
  cache-hit provenance gap without making cache availability a correctness
  condition.
- `activate-renderer.yml` is a main-owned manual activation producer. It
  attests the manifest and every supported platform asset, derives a
  short-lived readiness record that preserves product provenance separately
  from the requested pin, runs `promote` on an activation branch, and opens a
  protected PR. It cannot write activation directly to main.
- Focused policy, policy-rendering, promotion, readiness, and transport
  regressions have been exercised locally. The implementation is not yet
  activated into `.github`, protected-integrated, or fully hosted-verified.

## Remaining required work and demonstrated blocks

- Implement and connect the typed workflow graph: explicit event contexts,
  artifact producers/requirements, platform/build contracts, trust edges, and
  a bounded resolver state machine. Static graph checks must reject cycles,
  producer-less requirements, and impossible event/platform edges before
  execution.
- Complete the restricted candidate qualification lane and separately
  controlled publisher. Verify builder identity, approved revision, build
  inputs, artifact digest, platform/profile/features, run/attempt, retention,
  revocation, complete-manifest visibility, concurrent publishers, and
  rollback. Local manifest validation alone does not establish hosted
  publication trust.
- Regenerate all active consumers through the pinned toolchain, prove a clean
  second generation pass, and remove every obsolete polling/permissive path.
- Establish a trusted required-verdict authority that a PR cannot replace or
  spoof. The refreshed repository ruleset has required status names but no
  verified GitHub App/integration binding; this is an observed trust gap, not
  an assumed resolution.
- Run protected integration and current main/Preview workflows at the final
  reviewed SHA, including merge-group or equivalent prospective-main proof,
  fork/rerun identities, failed/cancelled/skipped obligations, and supported
  consumer platforms.
- Measure matched cold/warm qualification, publication, dependency wait,
  artifact transfer, and generator-change-to-activation paths. The observed
  hosted critical path is currently above 120s; no performance acceptance is
  claimed until causes are measured and feasible remedies are verified.
- Local actionlint is temporarily unavailable: the unauthenticated GitHub API
  rate limit returned HTTP 403 while `mise` attempted to install
  `actionlint@1.7.10`. This is an external tooling block, not a passing lint
  result; hosted actionlint still remains required.

Completion requires all items above, independent security/correctness review,
protected integration, hosted evidence, and an honest remaining-violation
report.
