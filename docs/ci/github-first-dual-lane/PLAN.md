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

This source graph is the durable contract. Mutable task ownership, revisions,
invalidations, and amendments live at the stable external session path
`/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/session.json`.
The current read-only G0 workload/platform projection is
`/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G0/workload-matrix.json`;
it covers all 32 manifest rows and does not claim generated G3 coverage or
execution success.

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

## Mandatory pull-request merge gate

No recovery, migration, release, or generated-output pull request may merge
until this complete gate passes for its exact candidate SHA. The owner and
independent reviewer must read all paginated reviews, issue comments, inline
threads, bot comments, requested changes, and feedback added after any fix.

1. Inspect the actual code/config/generated-output diff and the relevant tests;
   do not rely on review labels, summaries, or a green subset.
2. Fix every valid finding, including test and documentation findings. Rerun
   affected checks and inspect the final diff.
3. Record each rejected suggestion and its evidence in the external ledger.
4. Re-read the complete paginated review/comment/thread/bot/requested-change
   set after every fix or new feedback event. Verify required CI and the final
   candidate SHA.
5. Stop if any feedback or required result is unread, unverified, or
   actionable. Merge only with a complete disposition and final main-SHA
   record.

This is an execution prerequisite, not a gate-success claim. The authoritative
procedure and command boundary are in
[`RUNBOOK.md`](./RUNBOOK.md#mandatory-pr-merge-preflight).

## Bounded initial task queue

Every task has one owner, one input revision, explicit evidence, and a separate
reviewer. Unknown thread/worktree metadata stays `unknown` until observed.

| ID | Repository/component | Owner | Dependencies | Owned files/worktree | Acceptance commands | Evidence output | Reviewer |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `G0-inventory` | Fixed 32-repository fleet | `/root/g0_inventory` | none | External ledger inventory only; worktree `unknown` | `[pending]` live GitHub inventory with pagination; `git rev-parse HEAD` | `G0/inventory.json`, access and dependency records | `/root/g0-reviewer` |
| `G0-bootstrap` | Velnor generator/runtime bootstrap | `/root/g0_bootstrap` | `G0-inventory` findings as needed | Generator worktree `unknown`; no records in source | `[pending]` clean/shallow checkout bootstrap and pin/artifact checks | `G0/bootstrap.json` with source/artifact/output identities | `/root/g0-reviewer` |
| `G0-distribution` | Velnor, `velnor-apt`, `homebrew-velnor` | `/root/g0_distribution` | `G0-inventory` | Distribution investigation worktree `unknown`; external evidence only | `[pending]` release discovery/feed/formula inventory | `G0/distribution.json` and access gaps | `/root/g0-reviewer` |
| `G0-fleet` | Fleet categories/workload matrix | `/root/g0_fleet` | `G0-inventory` | Fleet worktree `unknown`; source edits prohibited in this wave | `[observed]` read-only 32-row workload/platform projection; exact emitted scanner IDs remain partial | External `G0/workload-matrix.json` plus fleet refresh files | `/root/g0-reviewer` |
| `G0-runtime` | macOS/OrbStack capability analysis | `/root/g0_runtime` | `G0-inventory` | Runtime investigation worktree `unknown`; no live host mutation | `[pending]` source capability and host-access checks | `G0/runtime-capabilities.json` | `/root/g0-reviewer` |
| `G0-records` | Canonical execution records | `/root/g0_records` | none | `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-records`; this directory's five docs only | `[verified]` RTK/version/git/model metadata; `[pending]` checker schema validation | These five source docs; external session ownership amendments | `/root/g0-reviewer` |
| `G0-checker` | Deterministic evidence checker | `/root/g0_checker` | `G0-records` schema | `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-checker`; checker-owned code/tests | `[rejected]` initial b3b6 unit hygiene passed but semantic review found false-green paths; v2 architecture/implementation/review remain pending | External checker review and hostile fixtures | `/root/g0-reviewer` |
| `G0-reviewer` | Independent G0 records/evidence review | `/root/g0_reviewer` | all initial outputs | Review-only worktree `unknown`; no author approval | `[pending]` fresh read of source docs and external raw evidence | Independent findings and disposition | `/root` |
| `G1-cache-semantics` | Hosted cache compatibility | `/root/g1_cache_semantics` (Luna/max) | `G0-inventory`, `G0-bootstrap` | Thread `01a0ba76-f725-7022-9cfa-f28456ab67b2`; external findings | `[observed]` stale fixture and PR953 cache-contract diagnosis; refresh pending | `G0/cache-semantics/findings.md` | `/root/g0-reviewer` |
| `G1-hosted-config` | Hosted-first generator policy | `/root/g1_hosted_config` (Luna/max) | `G0-inventory`, `G0-bootstrap` | Thread `01a0ba77-5222-7e63-97fa-553849b96d7b`; worktree `hosted→g1_hosted_config` | `[in progress]` typed config/regeneration/policy checks | G1 candidate/source/output identity | `/root/g0-reviewer` |
| `G1-review952` | PR #952 and stacked recovery work | `/root/g1_review_952` (Luna/max) | `G0-inventory`, `G1-hosted-config` | Thread `01a0ba77-dd88-7ac2-9fb7-118f3c09d1af`; worktree unknown | `[pending]` PR #952/#953/#954 review and candidate checks | PR disposition and post-merge requirement | `/root/g0-reviewer` |
| `G1-run-operations` | Existing failed run/child graph | `/root/g1_run_operations` (Luna/max) | `G0-inventory` | Thread `01a0ba7a-9d5d-7291-a2f5-357ff78dba5e`; owns external `G0/stale-runs.json` | `[observed]` failed-run/child reconciliation in progress | `G1/run-operations.json`, `G0/stale-runs.json` | `/root/g0-reviewer` |
| `G1-runtime-product-audit` | Published runtime product and promotion sequence | `/root/g1_run_operations` (Luna/max) | `G0-bootstrap`, `G1-seed-pin` | Same thread; external audit only | `[observed]` old pin/release verified; current main and unpublished candidate distinguished | `G1/runtime-product-audit/{runtime-product-audit.json,PROMOTION.md}` | `/root/g0-reviewer` |
| `G1-seed-pin` | Generator seed/pin reuse | `/root/g0_inventory` (Luna/max) | `G0-bootstrap`, `G0-inventory` | Thread `01a0ba72-3925-7141-b1f7-5529a5cf6c98`; worktree `generator→g0_inventory` | `[observed]` exact seed/pin review; clean pin adoption/regeneration pending | `G1/reviews/seed-pin.md` | `/root/g0-reviewer` |
| `G1-scan-integrity` | Generated-output/source scan integrity | `/root/g0_inventory` | `G0-bootstrap`, `G1-hosted-config` | Worktree `dual-lane-scan-integrity`; source edits gated/reviewed | `[observed]` candidate `6409a086` from parent `12cc87b` has 1740 source tests excluding expected stale snapshot plus fmt/clippy; approval pending exact G1-review952 | External `G1/scan-integrity/REPORT.md` and integration status | `/root/g1_review_952` |
| `G2-native-packages` | Native package/Homebrew prerequisites | `/root/g2_native_packages` (Luna/max) | `G1`, `G0-distribution` | Thread `01a0ba7a-328e-7282-943e-5b54c2ac209d`; worktree `dual-lane-native-packages` | `[observed]` three ARM64 macOS binaries compile/smoke only; worker now also owns product/manifest contract; install/publication pending | `G0/native-packages/findings.md` | `/root/g0-reviewer` |
| `G2-homebrew-contract` | Homebrew consumer/producer contract | `/root/g2_homebrew_contract` (Luna/max) | `G1`, `G0-distribution`, `G2-native-packages` | Thread `01a0ba80-8408-7380-8ac2-b743eb4494a5`; worktree `dual-lane-homebrew` | `[pending]` typed Homebrew contract and producer coordination | `G2/homebrew-contract/findings.md` | `/root/g2_distribution_review` |
| `G2-native-product` | Product binary/component identity and authoritative package manifest | `/root/g2_native_packages` (Luna/max) | `G1`, `G0-distribution` | Thread `01a0ba7a-328e-7282-943e-5b54c2ac209d`; worktree `dual-lane-native-product` | `[pending]` application/runtime component inventory, identity, and package contract | `G2/native-product.json` | `/root/g2_distribution_review` |
| `G2-preview-publication` | Immutable preview publisher/version/monotonic channel | `/root/g1_run_operations` (Luna/max) | `G1`, `G2-native-product`, `G0-distribution` | Thread `01a0ba7a-9d5d-7291-a2f5-357ff78dba5e`; worktree `dual-lane-preview-publication` | `[blocked]` design/source work may prepare, but no real publication before G1 | `G2/preview-publication.json` | `/root/g2_distribution_review` |
| `G2-distribution-review` | Independent APT/Homebrew product contract review | `/root/g2_distribution_review` (Luna/max) | `G2-native-product`, `G0-distribution` | Thread `01a0ba81-1af6-7f11-9f24-3ff115b8f314`; review worktree unknown | `[observed]` package identity/publication/install blockers recorded | `G0/distribution-review/report.md` | `/root/g0-reviewer` |
| `g3-skills-adapter` | Eight skills repositories: `tailrocks/tailrocks-typescript-skills`, `tailrocks/tailrocks-skill-authoring-skills`, `tailrocks/tailrocks-rust-skills`, `tailrocks/tailrocks-roadmap-skills`, `tailrocks/tailrocks-pull-request-skills`, `tailrocks/tailrocks-open-source-skills`, `tailrocks/tailrocks-macos-skills`, `tailrocks/tailrocks-code-quality-skills` | `/root/g3_skills_adapter` (Luna/max) | `G0-inventory`; operationally G3 depends on G2 | Thread `01a0ba7b-0152-7a70-8d18-c2387f1c9469`; external read-only source inspection | `[observed]` catalog/frontmatter/template inventory; central scanner fix required | `G0/skills-adapter/report.md`; no G3 rollout | `/root/g3_distribution_consumers` |
| `g3-native-routing` | Native Apple capability/routing: `tailrocks/tablerock`, `tailrocks/parallax-telemetry-playground`, `jackin-project/jackin` | `/root/g3_native_routing` (Luna/max) | `G0-inventory`; operationally G3 depends on G2 | Thread `01a0ba7b-32f1-7af1-a25f-4cde73f1f075`; worktree `dual-lane-native-routing` | `[observed]` actual routing gaps recorded; no rollout before G2 | `G0/native-routing/report.md`; no G3 rollout | `/root/g3_native_review` |
| `g3-action-roles` | Action/role images: `jackin-project/jackin-role-action`, `jackin-project/jackin-the-architect`, `jackin-project/jackin-sentinel` | `/root/g3_action_roles` (Luna/max) | `G0-inventory`; operationally G3 depends on G2 | Thread `01a0ba7b-57a5-7623-bd92-d0666a02b96e`; worktree `dual-lane-action-scanner` | `[observed]` action/role contract gaps recorded; scanner implementation pending | `G0/action-roles/findings.md`; no G3 rollout | `/root/g0_runtime` |
| `g3-rust-consumers` | Eight Rust/product consumers: `tailrocks/parallax`, `tailrocks/tracing-request-level`, `tailrocks/termrock`, `tailrocks/termpane`, `tailrocks/schemalane`, `tailrocks/ruxel`, `tailrocks/pg-bigdecimal`, `tailrocks/holla` | `/root/g3_rust_consumers` (Luna/max) | `G0-inventory`; operationally G3 depends on G2 | Thread `01a0ba7d-bb3f-78c3-84c8-eb0b2d75e5d0`; worktree `dual-lane-rust-scan` for central fix | `[observed]` manifests, tests, publishing, dependency-closure gaps recorded; termrock fix pending | `G0/rust-consumers/{report.md,inventory.tsv}`; no G3 rollout | `/root/g0-reviewer` |
| `g3-distribution-consumers` | Six non-Velnor feeds/taps: `tailrocks/homebrew-tablerock`, `tailrocks/homebrew-ruxel`, `tailrocks/homebrew-parallax`, `tailrocks/homebrew-holla`, `tailrocks/holla-apt`, `jackin-project/homebrew-tap` | `/root/g3_distribution_consumers` (Luna/max) | `G0-inventory`, `G0-distribution`; operationally G3 depends on G2 | Thread `01a0ba7d-e457-7723-81ba-1f7ed038212c`; external read-only audit | `[observed]` formula/feed/signature/update and clean-client gaps recorded; G3 blocked | `G0/distribution-consumers/{report.md,consumer-inventory.json}`; no G3 rollout | `/root/g0-reviewer` |
| `g3-native-review` | Independent native-routing review | `/root/g3_native_review` (Luna/max) | `g3-native-routing` | Thread `01a0ba8a-5bef-7d71-9a17-1d082a4f122a`; review worktree unknown | `[pending]` fresh review of native-routing evidence | `G0/native-routing/review.md` | `/root/g0-reviewer` |

The added G1/G2 rows are bounded follow-up tasks. Agent/thread metadata is
recorded in the external session ledger only after actual assignment and
turn-context verification; any remaining unknown is explicit.

The `g3-*` rows are early, read-only category audits. They may prepare findings
before G3, but cannot operate the fleet or merge migration changes before G2
exits. Their exact scoped repository lists are fixed above; compact records live
in the named external G0 subdirectories.

The checker workstream's contract is
`docs/ci/github-first-dual-lane/evidence-schema.md` in its worktree. The
canonical manifest fields are `schema_version: 1` and `manifest_id`; this
manifest uses those names only. `manifest_version` and `schema` are not
aliases accepted by this records contract. Preparation rows remain nullable
and fail G0 until live workload evidence replaces the unknowns.

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
