# GitHub-first dual-lane execution specification

Status: G0 is in progress. No gate is passed by this document.

This is the canonical source-tree specification for the execution described by
[`velnor-github-first-dual-lane-goal.md`](/Users/donbeave/Projects/tailrocks/velnor-project/velnor3/velnor-github-first-dual-lane-goal.md).
The goal document remains the authoritative user requirement. This record makes
its acceptance contract durable and machine-checkable; it does not turn an
unknown observation into a success.

## 1. Authority, snapshot, and evidence boundary

- Source repository: `tailrocks/velnor`.
- Initial source revision: `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`.
- Preparation date: 2026-09-19. Live state must be refreshed at every gate.
- Fixed fleet: exactly the 32 repositories in [`fleet.json`](./fleet.json).
- Live operational records: `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/`.
- The final ledger must be published as an immutable CI artifact or evidence
  ref. It must not be committed to this source tree after the SHA it attests.
- No credentials, raw logs, or unbounded artifacts belong in this directory.

The external session and ownership record is
`/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/session.json`.
After the G0 records amendment its SHA-256 is
`a60b01155210c8f6d5db9ddb48375c487baf4cbb788cb39116a9e6e3c5ebcd4f`.
It records the orchestrator, assigned agents, worktree ownership, current
gate, and the Velnor ruleset snapshot. Amendments to that record are durable
outside the source checkout and must retain an amendment history.

### Effective execution settings

These are observed settings, not a claim that the goal has passed any CI gate.

| Scope | Effective model | Effort | Evidence |
| --- | --- | --- | --- |
| Root session `01a0ba6f-f806-7d31-9abb-c828b3dc9e4e` | `gpt-6-astra` | `low` | `session.json`; local turn-context logs |
| This records agent `01a0ba74-0f82-7800-9937-fb9d22de7e3d` | `gpt-5.6-luna` | `max` | local `logs_2.sqlite` turn context |
| Configured subagent default | `gpt-5.6-luna` | `max` | `/Users/donbeave/.codex-chainargos/config.toml` |

Verified local metadata: Codex CLI `0.155.0`, RTK `0.49.0`, macOS `27.0`
(`arm64`, build `26A428`), configuration SHA-256
`bb217c6c00e6f5350d2ec0d2226973c895f5c749556924b8f289075a49d2bfac`, and
models-cache SHA-256
`3ab9ccbd7d392bd8e59caa2081a3e05ec46d80eca6778dfaf5a6d8e63c202d8c`.
The config permits 256 concurrent agent threads per session. Actual thread
availability and lifecycle state remain operational evidence, not a promise.

### Current Velnor policy observation

The external snapshot records active ruleset `19573071` for `tailrocks/velnor`
with required contexts `DCO`, `ci-required`, and `Policy`. No app binding or
ruleset transition has been claimed. Preserve neutral contexts and substantive
verification; prove the hosted-only contract on the exact candidate before any
required-check change.

## 2. Intended outcome and boundaries

Deliver a reproducible recovery and migration in this order:

1. Repair Velnor's generator and GitHub-hosted CI/release prerequisites.
2. Publish and install-test new preview and stable Velnor versions via Debian/APT
   and Homebrew, repairing `tailrocks/velnor-apt` and
   `tailrocks/homebrew-velnor` as required.
3. Migrate all 32 repositories to the reviewed generator and prove hosted PR
   and main CI.
4. Only after G3 passes, operate Velnor on the actual authorized macOS host
   through OrbStack, with every Velnor workload in Docker containers.
5. Repair, review, release, install, and reverify runtime/generator fixes,
   then generate and merge both providers across the fleet.

GitHub-hosted is the default and recovery provider. The final system runs both
providers automatically for eligible trusted PR/main workloads; explicit
provider selection remains available for diagnosis and recovery. A Velnor
failure must not silently fall back to hosted or be reported as Velnor success.

The scope includes generator/runtime code, typed configuration, workflow
outputs, packaging, distribution, release automation, tests, documentation,
and compatible required-check changes. It permits reviewed migration/fix PRs,
merges after gates, and required Velnor publication. It does not authorize
unrelated feature merges. Do not add a third provider, Scale Set deployment,
bastion/selene deployment, parallel generator, or competing runner controller.

## 3. Fixed fleet contract

The manifest is exactly 32 unique repository names. Default branches, revisions,
PR heads, checks, workload inventories, and evidence are unknown until live G0
inventory proves them. `fleet.json` uses `null` plus `evidence_status: "unknown"`
for facts not observed; `null` is not success and is not an exemption.

The fixed names are:

```text
tailrocks/velnor
tailrocks/velnor-apt
tailrocks/parallax
tailrocks/tracing-request-level
tailrocks/termrock
tailrocks/termpane
tailrocks/tablerock
tailrocks/schemalane
tailrocks/ruxel
tailrocks/pg-bigdecimal
tailrocks/parallax-telemetry-playground
tailrocks/homebrew-tablerock
tailrocks/homebrew-ruxel
tailrocks/homebrew-parallax
tailrocks/homebrew-holla
tailrocks/holla-apt
tailrocks/holla
tailrocks/homebrew-velnor
tailrocks/tailrocks-typescript-skills
tailrocks/tailrocks-skill-authoring-skills
tailrocks/tailrocks-rust-skills
tailrocks/tailrocks-roadmap-skills
tailrocks/tailrocks-pull-request-skills
tailrocks/tailrocks-open-source-skills
tailrocks/tailrocks-macos-skills
tailrocks/tailrocks-code-quality-skills
jackin-project/jackin
jackin-project/jackin-agent-smith
jackin-project/homebrew-tap
jackin-project/jackin-the-architect
jackin-project/jackin-sentinel
jackin-project/jackin-role-action
```

Any dependency outside this list is recorded as an out-of-scope dependency;
it does not silently enlarge the fleet.

## 4. Gates and sequencing

The graph is linear for operational rollout. Read-only investigation may run
ahead, but a downstream gate cannot pass early. A central generator/runtime or
publication defect reopens the affected earlier gate.

| ID | Entry/dependency | Required exit evidence | State at this snapshot |
| --- | --- | --- | --- |
| G0 | Initial checkout; no predecessor | 32 unique repositories; current default SHAs, PRs, checks, workflows, workload/platform matrix, dependency graph, access gaps, and effective model settings | In progress |
| G1 | G0 | Hosted generated policy, real required PR checks, and post-merge main CI pass without Velnor capacity | Pending |
| G2 | G1 | New preview and stable Velnor releases install and upgrade through APT and Homebrew; product and both distribution repositories pass hosted checks | Pending |
| G3 | G2 | All 32 use the approved generator; migration PRs and current main revisions pass hosted CI; open PR coverage reconciles | Pending |
| G4 | G3 | Actual Mac/OrbStack host identity, installation, routing, representative workloads, and hosted comparison are exercised; defects recorded | Pending |
| G5 | G4 | Defects fixed/reviewed/released/installed; complete pilot checklist passes with no unpublished-checkout dependency | Pending |
| G6 | G5 | Both generated providers merge and pass for every eligible workload on PR and resulting main; native-only checks retained | Pending |
| G7 | G6 | Independent audit, deterministic checker, current revisions/PRs/checks/package channels/runtime identities reconcile | Pending |

G4 and G5 are a repair loop. A failed pilot creates a bounded reviewed fix
and new package as needed; a passing installed pilot exits the loop. G6 may
reopen it for workload defects. No operational Mac rollout starts before G3.

## 5. Generator and hosted recovery acceptance

The following are mandatory requirements, not implementation suggestions:

- Snapshot every workflow entry point, reusable workflow/action, scanner unit,
  generated state file, status-check contract, trigger, release dependency, and
  relevant failure log. Classify each defect as product, generated output,
  bootstrap, external policy, or unavailable capacity.
- Use Velnor's actual source/config/schema and CLI help. At preparation the
  input was `.github-gen/velnor-workflow.toml`, schema 2; revalidate it. Change
  generator logic and typed declarations, then regenerate all output and
  sidecars together. Never hand-edit generated `.github` files, add repository
  conditionals to generic logic, or copy raw workflow templates.
- Automatic/default dispatch initially selects GitHub-hosted only while keeping
  the Velnor implementation available. Integration tests may run on hosted
  machines, but must really pass.
- A fresh checkout with an empty cache must obtain a verified pinned generator
  and runtime through a supported path. It cannot depend on an unreachable PR
  artifact, the broken release it is meant to create, mutable `latest`, or a
  developer checkout. Remove temporary source-build recovery assumptions before
  completion.
- Record generator source revision, artifact digest, consumer source revision,
  generated-tree digest, and scanner-input state separately. Use staged
  generator publication then pin adoption; prove clean-clone and shallow-clone
  regeneration and repair stale sidecars without weakening drift checks.
- Audit PR, push/main, dispatch, schedule, preview, stable-tag, and merge queue
  triggers. Preserve DCO, tests, security, and meaningful required checks.
  Transition required contexts only after equivalent or stronger hosted checks
  report from the correct App; never call hosted work a Velnor job.
- Follow dispatch wrappers through reusable/child runs. Aggregates fail for
  missing, failed, canceled, timed-out, or unexpectedly skipped expected work.
  Affected selection must have a full-unit baseline so empty/documentation-only
  matrices cannot hide broken builds.
- Verify cold/warm cache semantics, source-pin fetches, permissions, checkout
  depth, job dependencies, Docker/Buildx setup, versions, and package inputs.
  Bound retries/timeouts and diagnose repeated identical failures.
- Merge reviewed recovery only after candidate checks pass, then prove the
  resulting main revision. A green PR alone never satisfies G1.

## 6. Release and distribution acceptance

One release contract covers product identity, channel, source revision, version,
platform/architecture, binary inventory, package/formula names, assets,
checksums/signing, consumer update, and install commands. At minimum verify
Linux amd64/arm64 Debian and native Homebrew on the actual Mac; validate or
explicitly leave unresolved Intel/other advertised targets. Do not advertise a
target solely because cross-compilation produced an archive.

The installed product must contain or explicitly depend on the compatible
`velnorctl`, `velnor-runner`, and `velnor-workflow` roles. Every component emits
unambiguous product/source identity; component crate versions are not silently
presented as the product version. Runtime artifacts and application releases
use separate typed discovery namespaces. Runtime producers must not depend on
the application release that consumes them.

### Channel contract

| Property | Preview | Stable |
| --- | --- | --- |
| Source | Tested trusted main revision and unique build identity | Reviewed release revision and immutable version tag |
| Version | Unique orderable prerelease; never selected as stable accidentally | Valid monotonically advancing version |
| Release | Explicit prerelease and complete manifest/assets | Explicit stable and complete manifest/assets |
| APT | Deliberate preview selection | Stable by default |
| Homebrew | Explicit preview formula/package; `--HEAD` is insufficient | Explicit stable formula/package |
| Mutation | Immutable versioned artifacts; pointer advances after validation | Versioned assets/tag remain immutable |

Discovery tests cover paginated mixed releases, previews, runtime products,
incomplete/invalid assets, no eligible release, and API failure. Feed updates
preserve the other channel and retained recovery versions. Preview publishes
and validates immutable assets before advancing its index. Stable reruns verify
matching assets and never rewrite a tag or same-version bytes. Destination
locks prevent an older preview from overwriting a newer one.

Publication is singular: validate candidate binaries and staged feed/formula,
admit the exact source with required checks, publish immutable artifacts, update
APT and Homebrew, then run clean-client install/upgrade checks against real
endpoints. Postpublication checks are acceptance evidence, not prerequisites
for creating the first available release. A failed postcheck repairs forward
or restores an allowed channel reference without mutating immutable assets.

Required package evidence:

- Real new preview and stable workflow publications, not dry runs.
- Tags, release IDs, source commits, manifests, versions, identities,
  architectures, and asset digests agree.
- Debian metadata, dependencies, permissions, config/service definitions,
  signed indexes, scoped key configuration, and fresh APT install are valid.
- Stable-to-stable, preview-to-preview, and documented preview-to-stable switch
  preserve intended state; service tests run where a real service manager exists.
- Homebrew tap has meaningful generated PR/main CI, both channels, correct
  checksums/platforms, functional tests, and clean hosted-macOS installation.
- Installed commands perform useful smoke tests and find sibling binaries.
- Producer, consumer, publication, and install logs are linked per channel.
- If no prior preview exists, publish two unique real previews and test upgrade;
  do not fabricate history inside the checker.

## 7. Hosted fleet migration acceptance (G3)

Use one reviewed immutable generator epoch. For every repository build a
behavior inventory and map every former responsibility to generated output:
release/preview, feeds/taps, Renovate, schedules, signing/notarization,
artifacts, services, and tests. Remove superseded workflows only after a
verified replacement exists; do not retain duplicate pipelines.

Generic scanner/primitives gain regression fixtures for real routing, release,
and execution failures. A no-workflows marker, unsupported scan, empty matrix,
or missing history is not meaningful migration when code has behavior.

| Category | Required generated verification |
| --- | --- |
| Rust workspace/library | Format/lint/test/doc/example/feature checks, dependencies, and publishing contract |
| Polyglot/playground | Every discovered Rust, TypeScript/Bun, Gradle, Docker, database, telemetry, desktop, and E2E responsibility |
| APT repository | Feed generation/signatures/indexes, channel updates, provenance, fresh install |
| Homebrew repository | Formula/cask metadata, checksums, platform install/version tests, preview/stable behavior |
| Skills/plugin repository | Manifest/frontmatter/catalog, references, helpers, templates; distinguish examples from executable units |
| Jackin role image | Role validation, Dockerfile/image build, architectures, smoke test, dependency compatibility |
| GitHub Action repository | Metadata/shell checks plus consumer fixtures for success/failure, selection, build/no-build, Docker/Buildx |

Determine native capability from dependencies and code. Preserve Apple checks
for Jackin, Tablerock, and the playground; verify macOS version, Xcode/Swift,
SDK, deployment target, and architecture. A Linux Swift compiler or a macOS
label alone is not evidence. Migrate in dependency-aware waves and record
out-of-scope image/product dependencies.

Every repository needs byte-stable regeneration, meaningful local/static checks,
successful migration PR, deliberate full-workload hosted run, independent
review, reviewed merge, and CI on resulting main. Inventory all open PRs,
including drafts, bots, and forks, at G0/G3/G7. Unrelated failing feature work
or outside approval remains a named blocker. G3 is not complete while any
repository, required PR check, or meaningful workflow is inaccessible/missing.

## 8. Actual Mac/OrbStack runtime acceptance (G4-G5)

This phase must use the authorized current Mac, not a Linux development
container, another server, or a hosted Mac. Record macOS/architecture, host
identity, OrbStack/Docker versions and socket/context, server OS/architecture,
engine resources, Velnor package/source, image digest/platform, and all job
identities.

Use native macOS Velnor control/runner processes with Linux Docker workloads
through OrbStack. Do not provision Velnor-managed Firecracker/libvirt/runner
VMs. Do not impose per-container CPU/RAM limits; enforce one host-wide
`max_jobs=N` budget and bounded disk/cache retention. Do not alter unrelated
containers, volumes, services, or projects.

Begin with existing `velnorctl host`; verify registration scope/labels/groups,
shared capacity, trust policy, Docker resolution, and daemon defaults. Extend
it coherently for the fleet; do not assume one repository-scoped process claims
all repositories or that 32 independent instances enforce one global limit.

The complete pilot checklist is:

- **Identity/routing:** distinguish repository/event/PR/head/base/merge SHA;
  prove labels, groups, trust eligibility, offline behavior, hosted fallback,
  concurrent routing, deduplication, lease/heartbeat, terminal status, and
  cleanup. A manual run is diagnostic unless check association is proven.
- **Execution:** prove checkout, expressions, outputs, env/path files,
  reusable/composite/action support, post steps, artifacts, caches, and Docker.
  Compare generated graph and runtime capabilities.
- **Nested Docker/services:** prove health checks, networking, testcontainers,
  Docker actions, Buildx/BuildKit, private per-job daemon/boundary, job-to-job
  isolation, cleanup, and useful caches. Job A must not list/control/remove
  job B or unrelated user resources.
- **Capacity:** queue more than N jobs from multiple repositories; active jobs
  never exceed N, finite jobs complete, waiters retain leases, canceled waiters
  release permits, restart does not reuse stale permits, and Velnor admission
  is FIFO where it controls order. Do not claim GitHub scheduler FIFO.
- **Resources:** inspect engine capacity, retention, backpressure, and admission;
  confirm Velnor imposed no per-container CPU/RAM limits.
- **Cold/warm cache:** same source/target, empty/populated caches; verify tests,
  outcomes, compatibility, trust/architecture boundaries, corruption recovery,
  and intended invalidation.
- **Cancellation:** child processes, services, Docker actions, BuildKit, and
  post steps terminate boundedly; `always()`/`cancelled()` and remote terminal
  state agree; permits/resources release.
- **Recovery/connectivity:** interrupt isolated runner and restart; control a
  Docker disconnect/reconnect; reconcile without duplicate completion,
  publication, or orphan resources; verify supported host wake/reconnect
  behavior without forcing sleep/reboot.
- **Observability:** local/remote logs, exit/completion agreement, identity,
  image metadata, queue/duration/cache/retry/failure-class telemetry.
- **Trust:** forks/unknown PRs cannot access host credentials, publishing
  secrets, or privileged Docker. Eligibility derives from event, repository,
  and policy. Any authorization binds immutable SHA and expires on head change;
  labels/titles/PR-edited workflows alone cannot grant access.
- **Parity:** hosted and Velnor test the same source/workload/dependencies,
  fixtures, targets, and outcome; environmental differences are recorded.
- **Packaged operation:** released Homebrew package starts/runs/diagnoses,
  drains/stops/restarts, and cleans up. A development checkout does not pass.

Every defect gets a minimal reproduction, owning component fix, regression
test, independent review, failed-scenario rerun, and hosted/Velnor canary. If
G4-G6 changes runtime/generator/package/release behavior, publish as required,
reinstall, advance the generator epoch, regenerate affected consumers, and
refresh evidence. Final success cannot depend on unpublished source.

## 9. Dual-provider merge acceptance (G6)

Generate both lanes from the same typed provider model and logical verification
plan. Names and summaries identify provider and actual host; a job named
`Velnor` alone is not provenance. For every repository, run both applicable
lanes on the same PR integration candidate, independently review, merge, and
run both on resulting main. Preserve native-only checks and singular publishing.

If Velnor is offline or cannot admit a required job, the lane is pending/failing;
never silently fall back or mark it green. Required-check transitions preserve
fork/trust/affected selection/merge queue behavior. No empty matrix,
`continue-on-error`, skipped test, removed required job, or permissive
aggregate substitutes for passing work. Reconcile every current open PR and
leave unrelated feature blockers explicit.

## 10. Evidence schema and checker contract

Each row must contain these fields, with explicit applicability and unknown
status until observed:

```text
repository, repository_role, default_branch, default_branch_sha, observed_at_utc
generator_revision, runtime_product_id, generator_artifact_digest
configuration_digest, generated_tree_digest, scan_state_digest
runtime_release_version, runtime_source_sha, job_image_digest
expected_workload_ids, required_check_contexts_and_apps
workload_platform_architecture, provider_eligibility, justified_exclusions
PR_number, PR_head_sha, PR_base_sha, tested_merge_sha, merge_group_sha
workflow_path, workflow_revision, event, run_id, run_attempt, run_url
trigger_source_sha, actual_checkout_sha, provider, runner_name, host_id
expected_jobs, actual_job_ids, actual_job_conclusions, logs, child_run_links
release_channel, release_version, tag_target_sha, release_id, asset_digests
APT_feed_revision_suite_and_candidate, Homebrew_tap_revision_and_formula
install_upgrade_test_environment, installed_binary_identity, functional_result
owner, reviewer, gate_status, blocker, next_action
```

The checker validates exactly 32 coverage/uniqueness, pins/digests, current
revision correspondence, nonempty workload inventory, provider/platform,
required conclusions, child-run completion, check-context association, and
release/install evidence. It fails stale/missing/queued/canceled/timed-out/
unexpectedly-skipped/failed required work. It may accept an explicitly
justified non-applicable row only as non-executed evidence. Fixtures must cover
stale SHA, skipped job, missing repository, wrong provider, failed child, and
mismatched artifact. It must test cold/warm representative evidence without an
arbitrary soak duration.

## 11. Final completion checklist

The final report may say complete only if every applicable item passes:

- Scope: all exact 32 appear once.
- Sequence: hosted recovery and distribution precede hosted fleet; hosted fleet
  precedes Mac; pilot precedes dual-provider merges.
- Generation: source/pin/runtime/digest/scanner/output agree and reproduce.
- Coverage: all required behavior has generated replacement; missing categories,
  native routing, feed automation, and action behavior are resolved.
- Hosted CI: final migration PR and main jobs pass in every repository; no hidden
  Velnor prerequisite remains.
- Velnor CI: eligible jobs pass in OrbStack Docker on same contract; native-only
  work passes on correct hosted systems.
- PRs: current open PRs reconcile at declared snapshot; unresolved cases block.
- Distribution: preview/stable APT/Homebrew versions install, upgrade, switch,
  and function from published endpoints.
- Publication: product/runtime discovery, monotonic retry-safe channels,
  traceability, and one authority reconcile.
- Mac runtime: routing, identity, package, capacity, containers, logs,
  cancellation, restart, orphan cleanup, and cache behavior pass.
- Merges: resulting main runs exist; a green PR cannot replace post-merge run.
- Reconciliation: final fixes propagate into packages, installed runtime,
  generator epoch, and affected consumers.
- Independent review: another agent attempts to disprove completion from raw
  evidence; author does not approve own work.
- Deterministic gate: checker passes after fresh default tips/open PR heads.
- Operations: runbook contains tested regeneration, provider selection, release,
  install, host lifecycle, recovery, and rollback procedures.

Before G7, take a final UTC snapshot of every default tip and open PR. Any moved
revision, including an automatic release/version bump, invalidates affected
evidence. The final report must include a 32-row fleet table, preview/stable ×
APT/Homebrew delivery table, merged PR/release links, tested Mac identity and
configuration, and explicit blockers; verbose jobs stay in the immutable ledger.

## 12. Explicit initial blockers

At G0 start, the following are pending and must not be inferred green:

- Live default branches, SHAs, open PRs, checks, workflows, workload matrices,
  and access gaps for 31 repositories.
- Current Velnor hosted failure/recovery state and post-merge evidence.
- Preview/stable product artifacts, APT signed feeds, Homebrew formulas, and
  clean-client installation/upgrade evidence.
- Full-fleet generated migration and current-main evidence.
- Actual Mac/OrbStack reachability and packaged runtime pilot.
- Deterministic checker implementation and independent final review.
- Any ruleset transition or App binding beyond the recorded snapshot.

Preparation findings in the goal document (schema/provider pins, failed runs,
PRs #952-954, runtime/product release mix-up, Homebrew omissions, scanner
omissions, and native Apple routing) are leads to revalidate, not acceptance
evidence. This specification deliberately records them as hypotheses until
the relevant task attaches durable source/run evidence.

Read-only category evidence now attached under the external ledger is still
preparatory: `G0/skills-adapter/report.md`,
`G0/action-roles/findings.md`, `G0/rust-consumers/report.md`,
`G0/distribution-consumers/report.md`, and
`G0/distribution-review/report.md`. These reports identify scanner, product,
publication, and native-install gaps; they do not pass G0/G2/G3. The external
`G1/reviews/seed-pin.md` report keeps exact PR-head test evidence separate from
integrated-source verification, and `G1/reviews/bootstrap-hosted.md` is a
checkpoint only. No dirty owner worktree is approval evidence.

The external G1 runtime-product audit records a verified old generator pin
(`fdeed261`), immutable runtime closure/release (`81ba31f` / release
`391347842`), and a distinct current-main product closure (`63cea86`). The
local source candidate `12cc87b` has an unpublished closure (`1cbf31a`) and
must follow source admission, merged-main publication, immutable verification,
then pin adoption/regeneration. This ordering is evidence and a promotion
plan, not a G1 pass or permission to require an unpublished candidate product.
