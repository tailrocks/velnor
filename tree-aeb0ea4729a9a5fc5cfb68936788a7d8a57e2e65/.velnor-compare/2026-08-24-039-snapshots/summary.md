# Plan 039 Step 3 — read-only live snapshots + candidate release-ref closure (2026-08-24)

Read-only evidence only. No GitHub mutation, no Actions dispatches, no apply.
Capture method: `gh api` (authenticated, account `donbeave`) against
`api.github.com`; API version default. Raw captures in this directory.

## 1. Live runner-group snapshots

### tailrocks (`Tailrocks-groups.json`, group id 1 + 3)

| group | id | visibility | default | restricted_to_workflows | selected_workflows | runners | writable? |
|---|---|---|---|---|---|---|---|
| Default | 1 | all | true | false | [] | 0 | n/a (default) |
| velnor-trusted | 3 | selected | false | **false** | **[]** | 8 | yes (non-default, non-inherited, restrictions writable) |

Selected repositories (21): the 18 canonical Tailrocks repos + `velnor-actions`
+ **`cloudflare-tofu`** + **`github-terraform`**. The two terraform repos are
extra selections beyond what plan 039 records; they have no reviewed workflow
closure in this pass.

### ChainArgos (`ChainArgos-groups.json`, group ids 1, 3, 4)

| group | id | visibility | default | restricted_to_workflows | selected_workflows | runners | writable? |
|---|---|---|---|---|---|---|---|
| Default | 1 | all | true | false | [] | 0 | n/a (default) |
| Blacksmith runners 01KC9QASP0VQHCRGZY0NTY6492 | 3 | all | false | false | [] | 0 | third-party group, untouched by plan 039 |
| velnor-trusted | 4 | selected | false | **false** | **[]** | 4 | yes (non-default, non-inherited, restrictions writable) |

Selected repositories (6): 3 canonical ChainArgos repos + `velnor-actions` +
`github-terraform` + **`cloudflare-tofu`** (the latter beyond the plan's
recorded set; no reviewed closure).

### jackin-project (`jackin-project-groups.json`, group id 1 + 3)

| group | id | visibility | default | restricted_to_workflows | selected_workflows | runners | writable? |
|---|---|---|---|---|---|---|---|
| Default | 1 | all | true | false | [] | 0 | n/a (default) |
| velnor-trusted | 3 | selected | false | **false** | **[]** | 8 | yes (non-default, non-inherited, restrictions writable) |

Selected repositories (9): 7 canonical jackin-project repos +
`jackin-github-terraform` + `velnor-actions`. Matches plan 039's recorded set.

Drift verdict: all three `velnor-trusted` groups remain
`visibility=selected` but `restricted_to_workflows=false`,
`selected_workflows=[]` — identical to the 2026-08-24 snapshot recorded in the
plan. Contract violation persists in every org. No tokens or credential
material present in any capture; no redaction required.

## 2. Candidate workflow closure (`workflow-enumeration.tsv`)

All 28 canonical class-map repositories enumerated at their GitHub-reported
default branch; every one resolves to `main` (`refs/heads/main`). No throttle
gaps; nothing fabricated. 145 workflow files listed; candidates = workflows
containing the plural `lanes:` choice input or calling a pinned `ci-*.yml` /
`package-*.yml` reusable from a velnor-actions mirror: **136 candidates**.
Plus the pre-existing `tailrocks/velnor-actions ci-code.yml` seed = **137
ledger entries**, all `review_state="seed-pending-review"`.

Per-repo candidate counts (repo : candidates):
blockchain-nodes 3; jackin-agent-brown 5; java-monorepo 5; homebrew-tap 5;
jackin 17; jackin-agent-smith 8; jackin-dev 3; jackin-role-action 4;
jackin-sentinel 8; jackin-the-architect 8; holla 5; holla-apt 4;
homebrew-holla 4; homebrew-parallax 3; homebrew-ruxel 1; homebrew-tablerock 3;
parallax 8; parallax-telemetry-playground 1; pg-bigdecimal 2; ruxel 3;
schemalane 2; tablerock 3; tailrocks-skills 5; termrock 4;
tracing-request-level 2; velnor 3; velnor-actions-fixture 13; velnor-apt 4.

Excluded (no lanes surface, no velnor-actions reuse): jackin/rust-nextest.yml,
jackin-the-architect/publish-image.yml, jackin-role-action/publish.yml,
tablerock/{native,native-nightly,native-release}.yml, parallax/mcp-evals.yml,
fixture/{compat-public-unmerged,fixture-rust-check}.yml. tablerock native*
align with §12.4 Apple-packaging exception (GitHub-hosted, not CI lanes).

Remaining closure gap: the three velnor-actions mirror repositories themselves
(`tailrocks/velnor-actions`, `jackin-project/velnor-actions`,
`ChainArgos/velnor-actions`) are outside the 28-repo class map and were not
enumerated here. Estate callers pin their callables (ci-code, ci-native,
ci-tap, ci-apt, ci-fixture, package-signer, package-updater) to exact SHAs;
only the pre-existing tailrocks/velnor-actions ci-code.yml seed is in the
ledger. Closure is INCOMPLETE until those mirrors get their own discovery pass.

## 3. Digest generation — GAP (flagged)

`fleet plan` is offline-capable but no operator-sanctioned invocation path
exists yet: mise tasks are {actionlint, check, ci, deny, fmt, lint, test,
test-focused}; none executes the `velnor-tools` binary, and `cargo run` /
`rtk cargo` are outside this task's authorization. No digests computed and no
digests fabricated. Emitted instead as canonical-serialization INPUTS
(field order exactly matches `OrgPolicy` serde declaration order):

- `{tailrocks,ChainArgos,jackin-project}-desired-policy.json`
  - tailrocks: 19 repos (18 canonical + velnor-actions), 71 workflows
  - ChainArgos: 4 repos (3 canonical + velnor-actions), 13 workflows
  - jackin-project: 8 repos (7 canonical + velnor-actions), 53 workflows

These are proposed shapes for operator review only; they are NOT validated
policies (`validate_policy` would currently reject them because the
velnor-actions mirror repos lack ledger workflow entries) and carry NO digest.
Digest generation awaits an operator-sanctioned offline invocation path
(e.g. a dedicated mise task wrapping `fleet policy plan --policy ...`).

## 4. OPERATOR DECISION REQUIRED

Per plan 039 Step 3: "Operator reviews the exact plan digest before mutation.
Approval covers only the shown fields, repositories, workflows, and refs."
No digest exists yet (see gap above); therefore NO apply approval is requested
or possible in this state. Before any future apply, explicit operator approval
must cover, per organization (pilot Tailrocks first, then ChainArgos, then
jackin-project), exactly:

1. Group identity fields: `group_name=velnor-trusted`,
   `visibility=selected`, `allows_public_repositories=true`,
   `restricted_to_workflows=true`, runner label `velnor-target-mvp`.
2. The exact selected-repository set shown in the desired-policy JSON above —
   including a decision on the observed extras with no reviewed closure:
   `tailrocks/cloudflare-tofu`, `tailrocks/github-terraform`,
   `ChainArgos/cloudflare-tofu`, `ChainArgos/github-terraform` (keep requires
   adding their exact workflow/ref entries; otherwise removal diff).
3. The exact 137-entry workflow list at `refs/heads/main` after review of
   fleet/release-refs.toml (all entries currently seed-pending-review).
4. Completion of the velnor-actions mirror discovery pass (three repos,
   seven callable families) so repository selection can satisfy the
   direct-workflow-closure rule.
5. The printed plan digest at that time; any later change reopens review.

STOP conditions from the plan still active: ambiguous direct job ownership of
any reusable workflow, or any desired removal without reviewed closure
evidence, blocks reconciliation.

## Gates

- `rtk mise run test-focused -- -p velnor-tools fleet_policy`: exit 0 (34/34).
- `rtk mise run check`: exit 0 (workspace compiles incl. foreign WIP; 877/877
  tests; deny advisories ok). Foreign WIP in crates/velnorctl/src/lib.rs was
  not touched, staged, or included.

## Digests (2026-08-24)

Command: `rtk mise run fleet-plan` (new Plan 039 tooling-law task; runs
`cargo run -p velnor-tools --locked --quiet -- fleet-policy plan --policy <f>
--ledger fleet/release-refs.toml` per `*-desired-policy.json`).

- tailrocks -> sha256:0b8666bbb56fc6286ffa093edfeabf43673163187ae125e73ed7a97647cff64c

ChainArgos and jackin-project produce NO digest yet: their Step-3 drafts fail
strict policy validation because selected repo `<org>/velnor-actions` has no
`selected_workflows` entry ("extra selection") — the known mirror-callable
discovery gap (STOP condition above). ChainArgos additionally carries mixed
login casing (`chainargos/*`) in `selected_repositories`. Both drafts were
left byte-exact; no digest was fabricated. Gate exits: test-focused 0,
actionlint 0, check 0.

## Mirror pass (2026-08-24)

**Decision: option (b)** — each velnor-actions mirror remains a selected
repository AND its estate-consumed callables are listed as workflow
identities. Rationale: GitHub's `restricted_to_workflows` model requires the
called reusable workflow itself to be selected for the runner group (a caller
job only gets access when both the calling workflow and the called reusable
are admitted), and plan 039's contract says "include each approved reusable
workflow that directly defines a Velnor job; do not infer transitive access".
Option (a) (dropping mirrors) would make live apply deny every pinned
reusable call at runtime. Evidence: `gh api repos/<org>/velnor-actions` — all
three mirrors default branch `main`, identical 10-file workflow sets; estate
callers consume exactly seven families (ci-apt, ci-code, ci-fixture,
ci-native, ci-tap, package-signer, package-updater) per the SHA-pinned
`uses:` lines in `workflow-enumeration.tsv`. Mirror-local `ci.yml`,
`fanout-smoke.yml`, and `owner-fanout-canary.yml` have no reviewed
estate-consumer closure and are excluded (minimal valid policy).

**Ledger** (`fleet/release-refs.toml`, committed): 21 entries added
(3 mirrors × 7 callables, all `refs/heads/main`,
`seed-pending-review`); 0 removed; header gap paragraph replaced by the
mirror-pass rationale. 137 → 158 entries.

**Drafts regenerated** (jq canonical transform: casing normalize → sort →
append mirror identities → unique/sort_by path+ref; schema unchanged):
- ChainArgos: casing `chainargos/*` → `ChainArgos/*` (3 repository slugs);
  +7 workflows = 20; 4 repositories.
- jackin-project: +7 workflows = 60; 8 repositories.
- tailrocks: also completed (+6 workflows beyond the ci-code.yml seed = 77;
  same seven-family evidence) — deviation from the two-draft instruction,
  required so no org carries a knowingly incomplete closure into operator
  review.

**Digests (`rtk mise run fleet-plan`, exit 0):**

- ChainArgos -> sha256:db3edaa1e0f2e058708fb3310bfc5ca9eca8cbe1c71cdeb76e33fe7ab47f68c0
- jackin-project -> sha256:97b13ff43e2132fc92fb34cbea4e34bca9c1754457b2899ece08a858ed39571f
- tailrocks -> sha256:b9f497117c5a4d6bc13b48ac5dbc857de92f9465df06631fcd3d8cb516e8cd57

(The earlier tailrocks digest sha256:0b8666bb… is superseded by the completed
seven-family closure.)

Gates: test-focused -p velnor-tools fleet_policy exit 0; `rtk mise run
check` exit 0. Foreign WIP in crates/velnorctl/src/lib.rs untouched. No apply
approval requested; all entries remain seed-pending-review and digests are
review inputs only.

## Prerequisites-first decision (researched) — 2026-08-24

Decision (operator-delegated): Plan 039 executes prerequisites-first. The
`restricted_to_workflows=true` flip on the Velnor runner groups happens ONLY
after every observed runtime ref shape is allowlisted; flipping earlier risks
silent infinite queueing ("Waiting for a runner", control-plane enforced,
no documented drain grace).

Researched citations:

- Full path@ref entry format: GitHub documents `selected_workflows` /
  workflow-group restrictions as exact `WORKFLOW_PATH@REF` entries, best
  practice pinned to `refs/heads/main`
  (https://docs.github.com/en/rest/actions/self-hosted-runner-groups —
  "selected workflows" field; and
  https://docs.github.com/en/actions/how-tos/manage-runners/self-hosted-runners/manage-runner-groups).
- Called-workflow admission: a job that `uses:` a reusable workflow runs
  under the CALLED workflow's admission too — the caller being allowlisted
  is not sufficient
  (https://docs.github.com/en/actions/reference/workflows-and-actions/reusing-workflow-configuration
  + runner-group restriction behavior above).
- Silent-queue risk: non-matching workflow/ref never errors; it queues
  forever at "Waiting for a runner" (control-plane enforced; observed in
  `.velnor-compare/plan063-r2-queue-validation` evidence). No documented
  drain grace window exists for restricted groups.

Four prerequisites and status (2026-08-24):

1. Runtime-ref coverage audit BEFORE flip — DONE: `ref-coverage-audit.md`
   in this directory. 484 sampled Velnor-runner executions across 31 repos /
   3 orgs (~60 days); verdict GAPS: refs/pull/N/merge ×89, other-branch ×32,
   refs/tags/* ×1 would be denied by the current refs/heads/main-only ledger.
   Raw rows: `runtime-ref-hits.tsv`.
2. Scratch-group proof before production group restriction — PENDING:
   requires the next fixture control-plane validation window
   (`restricted_to_workflows=true` proven on a scratch group first).
3. Drain-window procedure — DOCUMENTED as an apply-time gate: any apply run
   must include a drain step (quiesce dispatches, wait for in-flight jobs,
   then flip) recorded in the plan file; enforced procedurally, not yet in
   tooling.
4. Allowlist regeneration mechanism — EXISTS: regeneration is the existing
   mise tasks (`rtk mise run fleet-plan` draft pipeline over
   `fleet/release-refs.toml`), extended to emit the audit-observed ref shapes
   once the operator approves ledger review states beyond seed.

Order of execution locked: audit → operator approval of expanded ledger →
scratch-group proof → drain-window apply on production groups.
