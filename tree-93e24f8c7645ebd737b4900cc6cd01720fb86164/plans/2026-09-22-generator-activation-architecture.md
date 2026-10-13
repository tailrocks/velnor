# Generator activation and CI artifact architecture

Status: incomplete. This is the authoritative decision record as of
2026-09-23. The local integration branch contains an uncommitted implementation
slice; activation, protected integration, and legacy removal are not proven.

Local branch: `codex/generator-activation-20260922` at `aa5f312`.
Declared active renderer: `37e7814995995c107fc704e62096fb86c4709bea`.

## Incident root causes

The Preview and Main incidents share one architectural failure: a consumer was
allowed to treat an untrusted or nonexistent producer as authority for active
workflow output.

- Preview accepted `TreeComparison::Candidate` when `head != base`. That
  inferred trust from commit inequality instead of carrying event and trust
  identity. Candidate output could pass a PR check while the declared active
  pin rendered a different tree.
- Main Policy searched for a `ci-pr.yml` `pull_request` run at the main head
  SHA. A squash/push SHA is not a PR-head producer identity. The consumer did
  not model producer existence or terminal failure, so it guessed for 900s and
  slept between API polls.
- Promotion proved local render/stamp atomicity, but not that every activated
  runtime product was complete, retrievable, immutable, provenance-bound, and
  ready before the active reference changed.

These conditions also permit stale event identity, partial publication, false
provenance, and invalid activation on other platforms.

## Decisions and invariants

1. **Active tree.** For `pull_request`, prospective integration/`merge_group`,
   main, Preview, and release consumers, the active tree is exactly the
   deterministic render of the declared authenticated renderer over the
   audited configuration and complete scan/static inputs. Candidate output is
   never active authority.
2. **Policy.** Policy validates that active tree and active pin. It does not
   execute or acquire a candidate renderer and does not infer success from a
   candidate artifact.
3. **Lifecycle.** The only promotion path is qualify → publish → activate.
   Qualification is disposable and restricted. Publication is a trusted,
   immutable, all-platform product with typed build identity, attestations,
   exact run/attempt provenance, complete manifest closure, digest, expiry,
   revocation, and retention evidence. Activation changes the pin and tree
   only after those facts pass.
4. **Identity and trust.** Event, source, audited/base/tree, policy, build,
   binary, plan, artifact, run, attempt, and result identities are distinct
   typed values. Fork and bot input cannot gain privileged trust or spoof a
   required verdict. A missing, failed, skipped, cancelled, stale, mismatched,
   or unverifiable obligation is failure. No-work success requires a positive
   proof of an empty obligation set.
5. **Workflow graph.** Event, trust, platform, artifact, and producer edges
   are explicit. Cycles, producer-less requirements, incompatible event
   edges, and impossible platform edges are rejected before execution.
6. **Performance.** Any applicable job, step, dependency wait, transfer, or
   critical path over 120s is a defect. Cold and warm measurements are both
   required.

## Lifecycle state

### Phase 1: bridge — current local state

The temporary main-only
`.github-gen/sources/workflows/runtime-products-bootstrap.yml` publishes a
typed three-platform runtime and v2 readiness record from protected main. Its
static mapping is intentional bridge machinery: it lets the old active pin
publish a product that the new activation contract can inspect.

The local active tree remains rendered by the declared `37e…` pin. Therefore
the checked-in generated `ci-main.yml`, `ci-policy.yml`, `ci-pr.yml`, and unit
workflows still contain the old candidate acquisition/publication path,
`--candidate-manifest`, and 900s/15s polling. This is an observed bridge state,
not an accepted final architecture.

The local `activate-renderer.yml` source validates the trusted publisher,
manifest and every platform asset, then calls promotion with readiness v2 and
the runtime-bootstrap retirement option. It cannot write main directly.

### Phase 2: atomic retirement — required, not executed

The generated publisher is the final authority. Before retirement it must emit
the same typed build, attestation, closure, and readiness-v2 contract as the
bridge publisher. One activation PR must atomically:

- switch the renderer pin;
- regenerate the complete active tree and ownership state;
- remove the bootstrap source and static mapping; and
- remove candidate acquisition, guessed polling, `candidate_publish`, and old
  runtime preparation/publication paths from active consumers.

Candidate rendering remains only in the isolated qualification workflow. The
local promotion code now has an atomic retirement path with rollback checks,
but no hosted activation has exercised it. Until Phase 2 is proven, this record
does not claim activation or legacy removal.

## Current local evidence

Recorded independent checks:

- The old pinned renderer regenerated the bridge tree; its `--plain --check`
  passed.
- The current candidate renderer produced byte-identical output on two clean
  renders.
- Selected generated/source workflows passed `actionlint`:
  `ci-runtime-products.yml`, `activate-renderer.yml`, `ci-main.yml`,
  `ci-policy.yml`, and `ci-pr.yml`.
- `cargo check --locked --all-targets --all-features` passed in the latest
  delegated check; `git diff --check` passed.
- Focused suites passed: IR 82, runtime 112, trust 15, platform 46,
  prepared-tool transport 42, promotion 7, consumer negatives 16, runtime
  products 60, and Swift scanner 53 tests.

The source slice now contains typed runtime build identity and manifest
filtering, readiness v2 validation, trusted activation checks, atomic bootstrap
retirement, event/trust validation, graph/platform checks, exact current-run
artifact admission, and canonical `SOURCE_SHA` generation for schema-2 and
release source paths.

These checks do not prove the final generated tree, full formatting/clippy
gates, hosted publication, protected integration, or rollback. The schema-2
`RunIdentity`/`ResultIdentity` APIs are still not wired into active execution
and aggregation. Independent review also found unresolved merge-group source
provenance and `pull_request_target` trust-policy gaps.

## Hosted snapshot

Read-only snapshot on 2026-09-23:

- Public `tailrocks/velnor` main was `0b8be6eec2b013e2f5364dc7b8ee0e05edce64e1`.
  There is no Preview branch; Preview runs from main.
- Ruleset [`protect-main`](https://github.com/tailrocks/velnor/rules/19573071)
  requires `DCO`, `Policy`, and `ci-required`, with squash-only/linear history
  and no bypass actors. Required contexts were not bound to a verified GitHub
  App/integration (`integration_id` was null).
- Main CI run
  [`35747674970`](https://github.com/tailrocks/velnor/actions/runs/35747674970)
  and runtime publication run
  [`35747674434`](https://github.com/tailrocks/velnor/actions/runs/35747674434)
  succeeded. Preview run
  [`35747675012`](https://github.com/tailrocks/velnor/actions/runs/35747675012)
  was still running; the preceding Preview run failed.
- Hosted main had no `activate-renderer.yml` or
  `runtime-products-bootstrap.yml`. Its active workflows still used pin
  `37e…`, candidate execution, `--candidate-manifest`, and 900s polling.
- The local branch was not pushed. Open PRs #1074 and #1076 were stale against
  the then-current main and did not provide activation evidence.

## Remaining gates and blockers

1. Wire complete typed run/result identity into selection, execution, receipts,
   and aggregation; bind repository, source/audited/base SHA, plan/command
   digest, provider/platform, run/attempt, trust, and outcome.
2. Close merge-group provenance, `pull_request_target` fork/bot trust,
   protected-ref dispatch, and tag/target event-scope parity. Add mutation
   tests for each fail-open class.
3. Make the generated publisher readiness-v2 equivalent to the bridge, then
   publish from the exact protected-main tip and verify all assets, attestations,
   retention, revocation, concurrent publication, and immutable release
   behavior.
4. Run Phase 2 promotion on a final reviewed SHA. Prove two clean generations,
   active-tree equality, ownership equality, actionlint, fmt, clippy, security,
   and full tests. Confirm no candidate/polling/runtime-legacy path remains in
   generated active consumers.
5. Complete protected integration and re-fetch final PR reviews, comments,
   replies, and threads. Exercise PR, merge-group/prospective-main, main,
   Preview, release, fork, rerun, and failed/skipped/cancelled obligation
   paths.
6. Produce matched cold/warm timing evidence with every applicable path at or
   below 120s. Run hosted rollback, activation-concurrency, and retention
   drills; document release retention. Current retained observations still
   include an approximately 18m30s main path, 788s Docker work, and 597s
   runtime publication.
7. Resolve the unbound required-verdict authority and the remaining non-green
   contract/formatting gates before claiming completion.

Completion means all gates pass with hosted evidence. Until then this is a
phase-1 bridge, not an activated or legacy-free implementation.
