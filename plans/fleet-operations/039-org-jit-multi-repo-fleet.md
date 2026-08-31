# Plan 039: Reconcile restricted organization JIT fleets

> **Executor instructions**: The marked unified-CI contract is authoritative.
> Produce and review an exact policy diff before changing GitHub. Do not add
> behavior to the retiring `velnor-runner` surface or duplicate commands owned
> by [`velnorctl-migration`](../velnorctl-migration/README.md).
>
> **Drift check**: re-read `VELNOR_PROJECTS_SETUP.md`, generated class and
> trusted-admission data, and current live `velnor-trusted` group/repository
> state for all three organizations. Runner registration/assignment state and
> full group-guard-state acceptance evidence are still pending gates; neither
> is completed capability. Historical counts below are evidence, not input.

## Status

- **Priority**: P0 security and contract correctness
- **Effort**: M
- **Risk**: HIGH (wrong policy can admit untrusted code or strand CI)
- **Depends on**: marked unified-CI contract; no implementation predecessor
- **Category**: fleet operations, security
- **Refreshed**: 2026-08-24

## Why work remains

Organization JIT code paths are present, but production acceptance is not
complete. `GitHubScope` builds current org JIT and runner-group endpoints;
configure/daemon resolve stable group names to ids; doctor inspects the
configured scope. Existing live evidence is limited to runner-group policy
readback. Runner registration/assignment state and full group-guard-state
acceptance remain explicit pending evidence, not completed capability.
`docs/org-fleet-migration.md` already owns drain/rollback operations. Do not
rebuild these features.

The remaining defect is policy drift. A bounded read-only GitHub snapshot on
2026-08-24 found all three `velnor-trusted` groups present and
`visibility=selected`, but the snapshot is partial group-policy evidence, not
runner-state or full guard-state acceptance. All three had:

```text
restricted_to_workflows=false
selected_workflows=[]
workflow_restrictions_read_only=false
```

This violates the current contract: trusted Velnor admission is limited to
exact default-branch or release workflow paths and refs, never a whole public
repository. Tailrocks currently selects the 18 canonical repositories plus
`tailrocks/velnor-actions`. ChainArgos selects its three canonical repositories
plus `github-terraform` and `velnor-actions`;
jackin-project selects its seven canonical repositories plus
`jackin-github-terraform` and `velnor-actions`. Retain an extra repository only
when the exact generated workflow closure proves it directly defines an
approved job.

Root cause: runner-group policy was a manual operation whose done gate checked
fleet availability, not exact workflow/ref admission. Fix the class with one
generated policy plus plan/audit/apply enforcement. A one-off PATCH is not done.

## Contract

- Estate membership/class comes only from the marked canonical class map: 18 Tailrocks,
  3 ChainArgos, and 7 jackin-project repositories.
- Each org uses non-default group `velnor-trusted`, `visibility=selected`, the
  existing runner label `velnor-target-mvp`, and
  `restricted_to_workflows=true`.
- Public unmerged contributor code stays GitHub-hosted. Group membership never
  overrides this route.
- Workflow identities are full
  `owner/repo/.github/workflows/file.yml@ref` values using qualified
  `refs/heads/...`, exact `refs/tags/...`, or full SHAs. No unqualified ref,
  glob, mutable release alias, or repository-wide substitute.
- GitHub grants only jobs directly defined in a selected workflow. Include each
  approved reusable workflow that directly defines a Velnor job; do not infer
  transitive access.
- Trusted and future lower-trust groups, daemons, labels,
  `VELNOR_TRUST_SCOPE`, stores, and Docker access remain separate.
- The velnorctl migration preserves org URL/group-name behavior before Plan 079
  deletes `velnor-runner`. Plan 080 owns product fleet views. Any helper here is
  maintainer-only in `velnor-tools`.

## Scope

In scope: generated exact policy, a Rust maintainer plan/audit/apply boundary,
one-org-at-a-time reconciliation, live denial/routing/warm proof, and removal of
stale org-fleet facts from active docs.

Out of scope: lower-trust execution, new Actions capability, marketplace
fallback, dynamic slots, multi-host scheduling, product fleet-view commands,
workflow workarounds, or a release/publish run used only as a smoke test.

## Steps

### 1. Generate one policy

Make the unified-CI generator emit deterministic machine-readable policy with,
per organization:

- group name and expected identity constraints
- runner selection label `velnor-target-mvp` plus any additional approved label
  proven by generated workflow closure; never infer a label from group name
- exact selected repository ids/names derived from approved workflow closure
- `visibility=selected`, `allows_public_repositories=true`, and
  `restricted_to_workflows=true`
- sorted, non-empty, duplicate-free exact workflow paths/refs
- contract/generator version

Resolve default branches from GitHub. Take release entries from the approved
release-ref ledger created by this plan under generated policy configuration.
The ledger schema records owner/repository, workflow path, allowed qualified
ref or full SHA, admission reason, approving change, and expiry/review state;
the unified-CI generator owns it and `audit-ci` validates it. If tag-triggered identity is unclear, prove the ref GitHub
reports with the fixture; never guess or widen. Generation fails on a missing,
extra, duplicate, wildcard, unqualified, or contradictory entry. Canonical CI
checks generated bytes.

### 2. Rust plan/audit/apply boundary

Policy generation has a separate filesystem trust boundary. The publisher
must run on Unix as effective uid 0 and write an absolute, root-owned output
directory whose ancestors and policy entries are not group/other-writable.
The generator rejects symlinked ancestors, symlink/hardlink/non-regular
entries, and writable policy entries. `flock` and descriptor-relative
revalidation detect unexpected churn but cannot provide pathname CAS against a
same-uid non-cooperating writer; see [Fleet-policy publication](../../docs/fleet-policy-publication.md).
Repository gates use read-only `audit-ci` and do not invoke this publisher.
The protection claim is limited to unprivileged/non-root writers subject to
Unix DAC; the dedicated root publisher, root or equivalent capabilities, ACL
exceptions, and the filesystem are trusted.

The maintainer-only `velnor-tools` fleet-policy operation provides:

1. `plan`: require `--policy` and `--ledger`, validate the approved release-ref
   ledger and policy match, then read live runner-group policy and selected
   repository state and print a deterministic, secret-free desired/observed
   diff; no mutation. It does not by itself prove runner registration,
   assignment, or full guard-state acceptance.
2. `audit`: fail nonzero on any desired group, repository, workflow, ref, or
   fail-closed guard mismatch; no mutation. Runner registration/assignment
   proof remains a separate Step 5 acceptance gate.
3. `apply`: require reviewed plan digest plus exact organization; reject stale
   digests; mutate only that org.

Use GitHub API `2026-03-10` or its documented successor. Apply the complete
workflow restriction, replace the exact repository set, then read both back and
require normalized semantic equality. Never emit tokens, credential-bearing
headers, or GitHub HTML.

Fake-API tests cover pagination, duplicate/missing groups, inherited/read-only
restriction, empty workflows, broad/narrow repo sets, stale digests, partial
failure, rate limits, redaction, idempotency, and readback disagreement.

Verify:

```text
rtk cargo nextest run -p velnor-tools fleet_policy
rtk mise run check
```

### 3. Review live diff

Save sanitized pre-change JSON under `.velnor-compare/`; never HTML. Require a
non-default, non-inherited group with writable workflow restrictions. Read back
every guard field before mutation; group ids are observations only and daemon
configuration keeps the stable name. This readback is required evidence, not a
completed runner-state or full guard-state acceptance claim.

Operator reviews the exact plan digest before mutation. Approval covers only
the shown fields, repositories, workflows, and refs. Changed digest means new
review.

### 4. Reconcile sequentially

Pilot Tailrocks because the fixture lives there; then ChainArgos; then
jackin-project. The current Tailrocks live selection has extra entries beyond
the generated closure, so its first plan is not assumed removal-free: apply
must stop until any removal has reviewed closure evidence. The desired set
contains the 18 canonical repositories plus `tailrocks/velnor-actions`, whose
approved reusable workflows directly define Velnor jobs. Repository allowlists
are the generated direct-workflow closure, not the 28-repository class map
alone.

For each org:

1. route new verification to GitHub-hosted; gracefully drain Velnor; prove no
   busy slot
2. apply workflow restriction first, then exact selected repositories
3. audit readback equality before resuming
4. confirm all new JIT registrations land in `velnor-trusted`, never Default
5. complete verification before touching the next org

Delete only exact stale/offline registrations owned by validation. Do not
delete healthy registrations for convenience.

### 5. Prove routing, denial, and warmth

Before every fixture/workflow smoke, cancel all older pending/in-progress runs,
remove only stale validation registrations, then dispatch. Record and monitor
only the new run id. Check within 60 seconds; diagnose no assignment or no state
change before two minutes.

Required proof:

- admitted Tailrocks fixture default-branch workflow reaches trusted group
- unlisted workflow/ref cannot reach it and is cancelled after bounded proof
- public unmerged contributor workflow uses GitHub-hosted; trusted group gets no
  assignment
- one non-writer admitted CI workflow per org completes; unchanged rerun proves
  dependency/tool/compiler cache reuse
- all three retain green doctor, group assignment, storage isolation, capacity,
  and GC
- generated policy and API readback are exact both directions

STOP if the release-ref ledger is absent, a reusable workflow's direct job
ownership is ambiguous, or the desired repository diff would remove a current
selection without reviewed closure evidence.

Never dispatch a writer/release just to test policy. Prove its configured entry
by readback; attach its next authorized run as later live evidence.

### 6. Prevent recurrence

- CI checks deterministic policy generation without GitHub credentials.
- Operator health schedule runs read-only live audit and reports drift; it never
  self-mutates.
- Apply remains manual and digest-gated.
- Refresh `docs/org-fleet-migration.md`, `docs/runner-usage.md`, and program
  evidence to remove July-era group/repository counts.
- Preserve the audit through velnorctl cutover; never retain the old binary as
  alias or policy authority.

## Done criteria

- [ ] Generated policy exhaustively names every approved repo/workflow/ref and
      rejects broad, missing, stale, or extra access.
- [ ] All three groups read back selected and workflow-restricted with exact
      non-empty workflows and exact repository closure.
- [ ] All live Velnor JIT runners belong to intended groups; none lands Default.
- [ ] Positive, negative, public-unmerged, and per-org warm proof has exact ids.
- [ ] Scheduled read-only audit detects later drift.
- [ ] Active docs contain no 13-repo, missing-group, per-repo-fleet, or
      incomplete-migration claim.
- [ ] `rtk mise run check` and task-relevant nextest suites pass.
- [ ] No fixture weakening, capability expansion, old-runner addition, or
      overlapping velnorctl command lands.

## Fail-closed / STOP

- Inherited or read-only workflow restriction: stop and name enterprise owner.
- Exact workflow/ref closure unavailable: stop; never fall back to repo-only.
- GitHub field/endpoint differs: inspect current official docs/behavior, update
  typed client/tests, and re-plan; never disable restriction.
- Required reusable workflow denied: add only its proven exact approved
  path/ref. Never select all workflows.
- Reconciliation strands jobs or degrades fleet: keep org drained, use explicit
  GitHub lane, diagnose, and repair exact policy. Never restore broad access.
- New capability, credential, network, storage, or trust surface needed: stop
  for explicit approval under strict capability contract.

## References

- `VELNOR_PROJECTS_SETUP.md`; `docs/{mission,vision,roadmap,prompt}.md`
- `docs/org-fleet-migration.md`; `docs/runner-usage.md`
- plan-039 row in `docs/program-requirement-evidence.tsv` (historical evidence)
- [GitHub runner-group REST API](https://docs.github.com/en/rest/actions/self-hosted-runner-groups?apiVersion=2026-03-10)
- [GitHub runner-group access management](https://docs.github.com/en/actions/how-tos/manage-runners/self-hosted-runners/manage-access):
  exact full workflow paths and pinned refs; only directly defined jobs get
  access

## Evidence (2026-08-24)

### Code-surface completion (steps 1, 2, 6 partial)
- Deterministic offline generation `fleet-policy generate`: byte-identical to reviewed snapshots for
  tailrocks, ChainArgos, jackin-project (`cmp` empty ×3; sha256 stable across regen) — commit `041079c`;
  idempotency + stale-org removal + symlink/case-collision fail-closed guards — `cf3ba76`, `080f1bb`,
  `8b45fe0`; review APPROVE verdicts recorded (final re-review APPROVE).
- Audit enforcement rules `fleet-policy-ledger/-current/-extra/-generate` + concrete malformed-TOML
  coverage — `041079c`, `cf3ba76`; audit-ci exits 0 on repo after lanes alignment — `8b8b4b6`; dual
  lane/lanes rejection hardening — `357410c`.
- Removal labeling: every observed-but-not-desired repository emits reason `no direct workflow closure
  in release-ref ledger`, pinned for github-terraform and jackin-github-terraform fixture cases —
  `357410c`.
- Gates: `rtk mise run check` exit 0 at each landing (workspace nextest 956→993 passing across slices;
  final focused suites fleet_policy 57/57, audit_ci green); `rtk mise run fmt/lint/actionlint/deny`
  exit 0.
- Independent verification C1–C9 VERIFIED at `041079c`; reviewer APPROVE ×3 (cf3ba76 findings fixed
  same-slice; residual collision-class LOWs closed in `8b45fe0`; re-review APPROVE).

### Step 6 docs truth
- org-fleet-migration.md hardcoded id lists + 21-repo allowlist table replaced by generated-policy/
  ledger authority pointers — `4d24356`.
- master-plan.md, target-action-registry.md, storage doc: 13-repository figures marked superseded by
  marked canonical class map — `faff2a2`.

### Execution reconciliation (plan-vs-reality, recorded before live steps)
- Anchor A: leaf step 4 premise "Tailrocks begins removal-free" is false against live capture
  (`.velnor-compare/2026-08-24-039-snapshots/summary.md:16-19`): live selection adds cloudflare-tofu +
  github-terraform beyond canonical 18 + velnor-actions. Apply produces a removal diff; STOP rule
  (removal without reviewed closure) honored — no live mutation attempted.
- Anchor B: leaf contract lines listing ChainArgos `github-terraform` / jackin
  `jackin-github-terraform` as selected conflict with generated closure (zero direct workflow entries;
  workflow-enumeration.tsv has none). Recorded as live drift to shrink pending explicit operator
  removal approval; diff now self-labels the reason.
- Anchor C: snapshots summary says "137 ledger entries"; committed ledger has 157 (mirror-callable pass
  eea87eb post-dated prose). Ledger file is authoritative.
- Anchor D: stale module doc claiming live plan/audit/apply unimplemented corrected in `041079c`.
- Ref-shape STOP stands: ledger admits only refs/heads/main identities; runtime evidence shows ~25% of
  recent Velnor runs on non-main refs (ref-coverage-audit.md GAPS verdict). Admission-shape resolution
  requires operator ruling before any restriction flip.

### Remaining for DONE (explicitly operator/live-gated)
Live steps 3–5: pre-change sanitized captures, operator digest review/approval per org, sequential
apply ×3, routing/denial/warm proof with run ids, scheduled-audit host enablement (repo-side systemd
unit slice queued), ledger seed approval after ref-shape ruling.

### Fresh read-only snapshot (2026-08-27T16:55Z)

Using the GitHub Actions runner-group API (`2026-03-10`), the bounded snapshot
found all three groups with `visibility=selected` and
`allows_public_repositories=true`, but `restricted_to_workflows=false` with
`selected_workflows=[]`:

- `tailrocks/velnor-trusted` (id 3): 21 selected repositories.
- `ChainArgos/velnor-trusted` (id 4): 6 selected repositories.
- `jackin-project/velnor-trusted` (id 3): 9 selected repositories.

The bounded audit performed no mutation. This is group-policy readback only; it
does not establish runner registration/assignment or full guard-state acceptance
evidence. Those remain pending. Plan 039 remains open; the workflow restriction,
exact closure, runner proof, and operator approval STOP conditions still apply.

The repository's live, read-only planner uses these desired-policy digests from
the current tree. It requires the release-ref ledger and a GitHub token, and
prints the live runner-group policy/repository diff alongside the summary and
digest. It does not replace the pending runner-state or full guard-state
acceptance evidence:

- `tailrocks`: `sha256:b9f497117c5a4d6bc13b48ac5dbc857de92f9465df06631fcd3d8cb516e8cd57`
- `ChainArgos`: `sha256:db3edaa1e0f2e058708fb3310bfc5ca9eca8cbe1c71cdeb76e33fe7ab47f68c0`
- `jackin-project`: `sha256:97b13ff43e2132fc92fb34cbea4e34bca9c1754457b2899ece08a858ed39571f`

Command: `rtk cargo run -p velnor-tools --locked -- fleet-policy plan --policy fleet/policies/<org>-desired-policy.json --ledger fleet/release-refs.toml`. The live plan requires every ledger entry to be currently approved and requires `GITHUB_TOKEN` or `GH_TOKEN`; it performs read-only runner-group/repository observation and does not authorize apply. These digests are planning evidence only and do not authorize apply.
