# Generator activation and CI artifact architecture

Status: active and incomplete. This is the authoritative execution record for
the generator activation, policy validation, and CI artifact dependency work.
Evidence was refreshed on 2026-09-22 against `origin/main` at
`45ef1ebef769c78f45315e11a798fdaafaef4c4e`; the reviewed implementation head
is PR #1044 at `ed3eab8015d28af2f7da23f852761a39687a5374`. Integration commits
are listed below. This record remains active and incomplete.

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
| Existing local `promote` transaction | Preserved; publication-readiness validation is enforced at its existing gate. No duplicate promotion mechanism. |
| `ci-runtime-products.yml` | Reused as an existing publication surface for investigation; its current lifecycle is not yet accepted as complete activation proof. |
| #978, #979, #980 drafts | Inspected; overlapping or unresolved bootstrap/prospective-main designs. None is treated as an implementation or merge authority. |
| Candidate-rendered PR exception and guessed `ci-pr.yml` acquisition | Superseded in the active source paths. The checked-in generated consumers still come from the old active pin until activation, so hosted proof must cover the regenerated activation increment. |
| Candidate qualification lane in `ed3eab8` | Implemented as an active-rendered static workflow that builds the candidate and renders twice into disposable directories without candidate credentials. Hosted run `35658419641` passed on job `106527617631`; the job ran for about 106s and took about 217s from event creation to completion. It is a qualification surface, not yet proof of hard restricted execution, trusted verdict authority, or publication. |
| `f7a73dc` staging attempt | Reverted by `d4651cf`; it changed active-generated configuration before a runtime product was published and exposed the old consumer's guessed-producer path. It remains failure evidence, not an accepted activation. |

## Evidence and timing

| Observation | Evidence | Result |
| --- | --- | --- |
| Preview incident | [run 35623133972, job 106410846252](https://github.com/tailrocks/velnor/actions/runs/35623133972/job/106410846252) | Generated-tree check failed in about 17–18s because candidate output differed from the active pin. |
| Main incident | [run 35623133871, job 106410848097](https://github.com/tailrocks/velnor/actions/runs/35623133871/job/106410848097) | Policy failed after about 920s; about 906s was absent-producer acquisition. |
| Recovery | Preview job `106420138590` and main Policy job `106420140784` | Passed in about 23s and 35s; these prove recovery, not complete lifecycle correctness. |
| Current main baseline | CI Main run `35629948234` | About 18m30s wall; Docker job about 788s and runtime publication about 597s. This remains a performance violation against the 120s requirement. |
| Current main pin reconciliation | `origin/main` `45ef1ebe` merged the D19 pin update to `eed474c4`; PR #1044 head `ed3eab8` includes that main reconciliation and retains `revision = eed474c4a1d9b071fd1b5de00c769c8997398e5a`. | The active renderer reference now matches current main's declared pin. This reconciles the hosted baseline before any activation attempt; it is not activation proof. |
| Foundation PR Policy | [run 35641013363](https://github.com/tailrocks/velnor/actions/runs/35641013363) | Passed in 29s at `cf2e236`; the active renderer validated the active tree without candidate acquisition. |
| Cache-proof PR Policy | [run 35643201030](https://github.com/tailrocks/velnor/actions/runs/35643201030) | Passed in 25s at `04c520d8`. The checked-in workflow is still rendered by the old active pin and executed its legacy candidate-acquisition step, so this is recovery evidence only. |
| Invalid staging activation | [Policy run 35650532054](https://github.com/tailrocks/velnor/actions/runs/35650532054) and [CI / PR run 35650535124](https://github.com/tailrocks/velnor/actions/runs/35650535124) | `f7a73dc` failed in about 1m52s: the staged configuration was invalid for the planning workflow, and the old generated Policy path then attempted candidate acquisition with no same-repository producer. This demonstrates why candidate output and active configuration cannot be advanced before publication. The change was reverted. |
| Static qualification implementation | [Policy run 35658417621](https://github.com/tailrocks/velnor/actions/runs/35658417621), [candidate run 35658419641](https://github.com/tailrocks/velnor/actions/runs/35658419641), [CI / PR run 35658420152](https://github.com/tailrocks/velnor/actions/runs/35658420152) | Policy passed at `ed3eab8`; the candidate job passed in about 106s, with about 217s from event creation to run completion. The CI / PR run was still live at evidence refresh, with child jobs queued or in progress. The candidate result establishes hosted scheduling for this head, but not hard network/filesystem isolation, a trusted required-verdict authority, or publication. Its event-to-completion path exceeds the 120s requirement and remains a performance breach. |
| Foundation final-SHA CI | [run 35641107449](https://github.com/tailrocks/velnor/actions/runs/35641107449) | Cancelled after two jobs were stuck far beyond 120s: `Set up Mr. Boxington` and `Clippy check`. This is retained as a hosted performance failure, not a green verification. |
| Artifact-fanout regression | [run 35647988900](https://github.com/tailrocks/velnor/actions/runs/35647988900) | Planning finalized artifact `10661426022`; two consumers received Results-service intermediary HTTP 403 while peers downloaded the identical artifact. This proves the per-consumer artifact fanout was a structural availability defect. |
| Fanout-free source check | [run 35649490883](https://github.com/tailrocks/velnor/actions/runs/35649490883) | All completed formerly affected download lanes passed. The run later failed only from runner-priority cancellation; it is not activation or performance acceptance evidence. |
| Current policy after merge | [run 35649789413](https://github.com/tailrocks/velnor/actions/runs/35649789413) | Passed at `cf63bde`; this validates the active-pinned tree only. |

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
- PR #1044 additionally contains `8f3e636`, `09fb262`, `cf63bde`, and
  `2d9bd07`: static activation-workflow ownership, removal of active-runtime
  artifact fanout, readiness lint proof, and current-main integration.
- PR #1044 at `d4651cf` adds a static candidate-qualification workflow. It
  builds the proposed renderer in the pull-request worker, renders twice into
  disposable trees, and compares those trees without changing the active
  generated tree. The workflow does not yet establish a hard network/filesystem
  sandbox or an independently controlled required-verdict authority.
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

- Complete and connect the typed workflow graph: explicit event contexts and
  typed same-run artifact requirements are present, but the resolver state
  machine and complete event/platform/trust validation are not yet proven over
  every active path. Static graph checks must reject cycles, producer-less
  requirements, and impossible event/platform edges before execution.
- Harden and connect the candidate qualification lane and separately
  controlled publisher. The static lane exists, but its introducing PR had no
  emitted hosted qualification run, and its current worker restrictions do not
  prove hard network/filesystem isolation. Verify builder identity, approved
  revision, build inputs, artifact digest, platform/profile/features,
  run/attempt, retention, revocation, complete-manifest visibility, concurrent
  publishers, and rollback. Local manifest validation alone does not establish
  hosted publication trust.
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
  artifact transfer, and generator-change-to-activation paths. The hosted
  candidate job is about 106s, but its event-to-completion path is about 217s;
  the live CI / PR run has not supplied a completed critical-path measurement.
  The observed hosted path therefore remains above 120s, and no performance
  acceptance is claimed until causes are measured and feasible remedies are
  verified.
- Local actionlint is temporarily unavailable: the unauthenticated GitHub API
  rate limit returned HTTP 403 while `mise` attempted to install
  `actionlint@1.7.10`. This is an external tooling block, not a passing lint
  result; hosted actionlint still remains required.

Completion requires all items above, independent security/correctness review,
protected integration, hosted evidence, and an honest remaining-violation
report.
