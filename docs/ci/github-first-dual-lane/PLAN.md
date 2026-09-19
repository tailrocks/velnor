# GitHub-first dual-lane plan

Current gate: `G0` (inventory and execution setup). Input revision for this
initial wave: `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`. No task may claim a
gate exit without durable evidence under the external ledger root
`/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/`.

## Durable gate graph

```text
G0 inventory/setup
 ├─> G1 hosted recovery
 │    └─> G2 preview/stable APT + Homebrew delivery
 │         └─> G3 complete hosted fleet
 │              └─> G4 actual macOS/OrbStack pilot
 │                   └─> G5 packaged pilot repaired and passed
 │                        └─> G6 dual-provider fleet
 │                             └─> G7 independent audit
 └─> G0 evidence/checker/reviewer tasks (parallel, no gate bypass)

G4 <────────────── repair loop ──────────────> G5
G6 may reopen G4/G5 when a fleet workload exposes a runtime defect.
```

The graph is a dependency graph, not a success claim. Read-only discovery can
start early; operational actions obey the arrows.

## Gate exit contracts

| Gate | Depends on | Exit condition | Required evidence owner |
| --- | --- | --- | --- |
| G0 | — | Exact 32 unique rows, live inventory, settings, access/dependency gaps | `/root`; G0 inventory + records |
| G1 | G0 | Hosted-only recovery PR and resulting main execution pass | G1 hosted-config/run-operations |
| G2 | G1 | Real preview/stable package delivery and clean install/upgrade checks | G0 distribution + G2 package owner |
| G3 | G2 | All 32 hosted migration PR/main revisions pass | G0 fleet + migration owners |
| G4 | G3 | Actual host identity/routing/workload comparison and defects | G0 runtime |
| G5 | G4 | Released installed pilot passes full checklist | G0 runtime + release owner |
| G6 | G5 | Both lanes pass for eligible fleet workloads and resulting mains | Fleet migration owners |
| G7 | G6 | Fresh revision/PR audit and deterministic checker + independent review | G0 checker + G0 reviewer |

## Bounded initial task queue

Every task has one owner, one input revision, explicit evidence, and a separate
reviewer. Unknown thread/worktree metadata stays `unknown` until observed.

| ID | Repository/component | Owner | Dependencies | Owned files/worktree | Acceptance commands | Evidence output | Reviewer |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `G0-inventory` | Fixed 32-repository fleet | `/root/g0_inventory` | none | External ledger inventory only; worktree `unknown` | `[pending]` live GitHub inventory with pagination; `git rev-parse HEAD` | `G0/inventory.json`, access and dependency records | `/root/g0-reviewer` |
| `G0-bootstrap` | Velnor generator/runtime bootstrap | `/root/g0_bootstrap` | `G0-inventory` findings as needed | Generator worktree `unknown`; no records in source | `[pending]` clean/shallow checkout bootstrap and pin/artifact checks | `G0/bootstrap.json` with source/artifact/output identities | `/root/g0-reviewer` |
| `G0-distribution` | Velnor, `velnor-apt`, `homebrew-velnor` | `/root/g0_distribution` | `G0-inventory` | Distribution investigation worktree `unknown`; external evidence only | `[pending]` release discovery/feed/formula inventory | `G0/distribution.json` and access gaps | `/root/g0-reviewer` |
| `G0-fleet` | Fleet categories/workload matrix | `/root/g0_fleet` | `G0-inventory` | Fleet worktree `unknown`; source edits prohibited in this wave | `[pending]` scanner/config/workflow inventory for all 32 | `G0/fleet-matrix.json` | `/root/g0-reviewer` |
| `G0-runtime` | macOS/OrbStack capability analysis | `/root/g0_runtime` | `G0-inventory` | Runtime investigation worktree `unknown`; no live host mutation | `[pending]` source capability and host-access checks | `G0/runtime-capabilities.json` | `/root/g0-reviewer` |
| `G0-records` | Canonical execution records | `/root/g0_records` | none | `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-records`; this directory's five docs only | `[verified]` RTK/version/git/model metadata; `[pending]` checker schema validation | These five source docs; external session ownership amendments | `/root/g0-reviewer` |
| `G0-checker` | Deterministic evidence checker | `/root/g0_checker` | `G0-records` schema | `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-checker`; checker-owned code/tests | `[pending]` checker unit/fixture tests for stale SHA, skip, missing repo, wrong provider, child failure, artifact mismatch | Checker commit and test report | `/root/g0-reviewer` |
| `G0-reviewer` | Independent G0 records/evidence review | `/root/g0_reviewer` | all initial outputs | Review-only worktree `unknown`; no author approval | `[pending]` fresh read of source docs and external raw evidence | Independent findings and disposition | `/root` |
| `G1-cache-semantics` | Hosted cache compatibility | `unknown` (Luna/max required) | `G0-inventory`, `G0-bootstrap` | Worktree `unknown` | `[pending]` cold/warm and cache-key semantic tests | `G1/cache-semantics.json` | `/root/g0-reviewer` |
| `G1-hosted-config` | Hosted-first generator policy | `unknown` (hosted worktree) | `G0-inventory`, `G0-bootstrap` | Worktree `hosted→g1_hosted_config` | `[pending]` typed config/regeneration/policy checks | G1 candidate/source/output identity | `/root/g0-reviewer` |
| `G1-review952` | PR #952 and stacked recovery work | `unknown` | `G0-inventory`, `G1-hosted-config` | Worktree `unknown` | `[pending]` PR #952/#953/#954 review and candidate checks | PR disposition and post-merge requirement | `/root/g0-reviewer` |
| `G1-run-operations` | Existing failed run/child graph | `unknown` | `G0-inventory` | Worktree `unknown`; owns external `G0/stale-runs.json` | `[pending]` failed-run and child-run reconciliation | `G1/run-operations.json`, `G0/stale-runs.json` | `/root/g0-reviewer` |
| `G1-seed-pin` | Generator seed/pin reuse | `/root/g0_inventory` | `G0-bootstrap`, `G0-inventory` | Worktree `generator→g0_inventory` | `[pending]` exact seed/pin provenance and shallow checkout proof | Pin/seed reuse report | `/root/g0-reviewer` |
| `G1-scan-integrity` | Generated-output/source scan integrity | `/root/g0_inventory` | `G0-bootstrap`, `G1-hosted-config` | Worktree `dual-lane-scan-integrity`; source edits gated/reviewed | `[pending]` remove self-invalidation while preserving real source drift checks | `G1/scan-integrity.json` | `/root/g1_review952` |
| `G2-native-packages` | Native package/Homebrew prerequisites | `unknown` (Luna/max required) | `G1`, `G0-distribution` | Worktree `unknown` | `[pending]` hosted macOS packaging and native binary smoke tests | G2 package capability report | `/root/g0-reviewer` |
| `G2-native-product` | Product binary/component identity and authoritative package manifest | `unknown` (Luna/max required) | `G1`, `G0-distribution` | Worktree `dual-lane-native-product` | `[pending]` application/runtime component inventory, identity, and package contract | `G2/native-product.json` | `/root/g2_distribution_review` |
| `G2-distribution-review` | Independent APT/Homebrew product contract review | `/root/g2_distribution_review` | `G2-native-product`, `G0-distribution` | Review worktree `unknown` | `[pending]` fresh review of package identity, channels, and publication chain | Independent G2 findings | `/root/g0-reviewer` |
| `g3-skills-adapter` | Eight skills repositories: `tailrocks/tailrocks-typescript-skills`, `tailrocks/tailrocks-skill-authoring-skills`, `tailrocks/tailrocks-rust-skills`, `tailrocks/tailrocks-roadmap-skills`, `tailrocks/tailrocks-pull-request-skills`, `tailrocks/tailrocks-open-source-skills`, `tailrocks/tailrocks-macos-skills`, `tailrocks/tailrocks-code-quality-skills` | `unknown` (Luna/max required) | `G0-inventory`; operationally G3 depends on G2 | External `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G0/g3-skills-adapter/`; read-only source inspection | `[pending]` manifest/frontmatter/catalog/reference/template inventory | Skills adapter findings; no G3 rollout | `/root/g0-reviewer` |
| `g3-native-routing` | Native Apple capability/routing: `tailrocks/tablerock`, `tailrocks/parallax-telemetry-playground`, `jackin-project/jackin` | `unknown` (Luna/max required) | `G0-inventory`; operationally G3 depends on G2 | External `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G0/g3-native-routing/`; read-only source inspection | `[pending]` actual SDK/toolchain/deployment/architecture and hosted-image inventory | Native routing findings; no G3 rollout | `/root/g0-reviewer` |
| `g3-action-roles` | Action/role images: `jackin-project/jackin-role-action`, `jackin-project/jackin-the-architect`, `jackin-project/jackin-sentinel` | `unknown` (Luna/max required) | `G0-inventory`; operationally G3 depends on G2 | External `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G0/g3-action-roles/`; read-only source inspection | `[pending]` action metadata, shell, Dockerfile, image/arch/smoke inventory | Action/role findings; no G3 rollout | `/root/g0-reviewer` |
| `g3-rust-consumers` | Eight Rust/product consumers: `tailrocks/parallax`, `tailrocks/tracing-request-level`, `tailrocks/termrock`, `tailrocks/termpane`, `tailrocks/schemalane`, `tailrocks/ruxel`, `tailrocks/pg-bigdecimal`, `tailrocks/holla` | `unknown` (Luna/max required) | `G0-inventory`; operationally G3 depends on G2 | External `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G0/g3-rust-consumers/`; read-only source inspection | `[pending]` manifests, tests, publishing, dependency-closure and workflow inventory | Rust consumer findings; no G3 rollout | `/root/g0-reviewer` |
| `g3-distribution-consumers` | Six non-Velnor feeds/taps: `tailrocks/homebrew-tablerock`, `tailrocks/homebrew-ruxel`, `tailrocks/homebrew-parallax`, `tailrocks/homebrew-holla`, `tailrocks/holla-apt`, `jackin-project/homebrew-tap` | `unknown` (Luna/max required) | `G0-inventory`, `G0-distribution`; operationally G3 depends on G2 | External `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G0/g3-distribution-consumers/`; read-only source inspection | `[pending]` formula/feed/signature/update and clean-client inventory | Distribution-consumer findings; no G3 rollout | `/root/g0-reviewer` |

The added G1/G2 rows are bounded follow-up tasks. Their agent/thread metadata
must be added to the external session ledger only after actual assignment and
turn-context verification; placeholders are intentional.

The `g3-*` rows are early, read-only category audits. They may prepare findings
before G3, but cannot operate the fleet or merge migration changes before G2
exits. Their exact scoped repository lists are fixed above; compact records live
in the named external G0 subdirectories.

The checker workstream's contract is
`docs/ci/github-first-dual-lane/evidence-schema.md` in its worktree. This
manifest keeps both `schema_version: 1`/`manifest_id` and the existing
`manifest_version: 1`/`schema` spelling so the checker can consume it while
preparation rows remain nullable and fail G0 until live workload evidence is
attached.

## Ownership and mutation rules

1. `/root` is integration/publication owner. It serializes generator pin
   adoption, release/tag/feed/tap mutation, shared ledger amendments, merges,
   and final snapshots.
2. `/root/g0_records` owns only the five files in this directory plus the
   explicitly assigned external session record. No source code, generated
   `.github` output, or checker implementation is edited here.
3. `/root/g0_checker` owns checker code/tests in its separate worktree. It
   consumes this manifest schema and cannot rewrite evidence to make it pass.
4. Investigators write compact records outside source. Raw logs remain outside
   both trees; links, hashes, and concise findings enter the immutable ledger.
5. Every evidence row identifies source SHA, event semantics, actual checkout
   SHA, provider, runner/host, expected/actual jobs, and reviewer. Missing data
   remains an explicit blocker.

## Phase work after G0

### G1 — hosted recovery

Repair generator/provider configuration, bootstrap, sidecars, cache semantics,
triggers, aggregate failure propagation, source pins, permissions, checkout
depth, Docker/Buildx setup, required-check transition, and stale-run analysis.
Prove exact candidate PR checks and resulting main CI on hosted runners.

### G2 — release and distribution

Implement the typed product/runtime discovery and channel contract, preview then
stable publication, signed APT metadata for amd64/arm64, Homebrew stable/preview
formulas and CI, clean-client install/upgrade/switch tests, and retry-safe
singular publication. Keep runtime artifacts out of application discovery.

### G3 — hosted fleet

Inventory then migrate by dependency-aware waves. Extend generic scanner and
primitives for missing categories; preserve native Apple checks and package,
feed, skill, action, and image behavior. For every row prove deterministic
regeneration, migration PR, full workload hosted run, reviewed merge, and main.

### G4/G5 — actual host and repair loop

Install the published package on the authorized Mac, record identity and
OrbStack/Docker data, prove routing, trust, capacity, nested Docker isolation,
cache, cancellation, recovery, connectivity, observability, parity, and
packaged lifecycle. For every defect create reproduction/fix/regression/review
and rerun the affected hosted and Velnor canaries, then republish and reinstall.

### G6/G7 — dual fleet and final audit

Generate both lanes from one typed provider model, run both on the same source
object and logical workload, merge only after checks/review, prove resulting
main, preserve native-only and singular publishing, reconcile all open PRs,
then refresh default tips and invoke the independent checker/reviewer.
