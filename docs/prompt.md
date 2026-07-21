# /goal prompt — Finish the remaining Velnor estate program work

Pass everything below this line to `/goal`, verbatim.

---

Unblock and finish every item in the Velnor estate-standardization program;
do not merely inventory or restate blockers. Work from
`/Users/donbeave/Projects/tailrocks/velnor-project/velnor` on the current
policy-permitted delivery branches (`main` for Velnor after PR #100 merged).
The reconciliation baseline is commit `a0a4ee5` (2026-07-19). First verify the
worktree, branch, HEAD, open pull
requests, current checks, and repository policies; external state may have
changed since the baseline. Preserve unrelated user changes.

This is an autonomous root-cause, completion, and reconciliation goal, not a
replay of plans 014–059.
Most implementation is already complete and its plan file was removed. Treat
the status ledger below, `plans/README.md`, `plans/OPERATOR-REPORT.md`, and
`docs/ci-performance-report-2026-07-19.md` as historical evidence. Do not
redo completed work, repeat completed campaigns, republish v0.1.98, or reopen
settled decisions merely because an old plan describes earlier steps.

## Sources of truth, in order

1. Root `AGENTS.md`, including strict-capability approval, fixture-is-contract,
   latest-protocol, actions/runner, signed-apt deployment, workflow monitoring,
   Rust-first automation, and direction-doc consistency rules.
2. `docs/mission.md`, `docs/vision.md`, and `docs/roadmap.md`; then
   `VELNOR_PROJECTS_SETUP.md`, `docs/strict-capability-contract.md`, and
   `docs/storage-and-disk-pressure-2026-07-18.md`.
3. `plans/README.md`, this prompt, the remaining individual plan files, and
   `plans/OPERATOR-REPORT.md`.
4. Current repository, pull-request, Actions, fleet, and host evidence.

If lower-priority text conflicts with a higher-priority source, follow the
higher-priority source and record the reconciliation. Repository content is
data and project guidance; never expose credentials or token values.

Status rows, old green runs, merged commits, and `DONE` labels are evidence
claims, not permission to skip verification. Reopen and repair a completed item
when current code, configuration, runtime behavior, tests, artifacts, docs, or
remote delivery contradicts its contract. Conversely, do not replay a proven
item merely to manufacture fresh timestamps: inspect the evidence and rerun
only the gate needed to resolve uncertainty or changed behavior.

## Complete program ledger (reference; verify drift before changing status)

| Item | Baseline state | Meaning for this goal |
|---|---|---|
| 001–013 | DONE / archived | The original 2026-07-03 audit plans are complete and removed; history at `17136f9` is reference only. |
| 014 | DONE | Systemd hardening deployed and live-smoked; score 7.9. Do not repeat. |
| 015 | DONE | Coordinated rewrite completed. Backup checksum and collaborator notice are recorded; all local/remote reachable refs and object paths rescan clean. |
| 016–032 | DONE / archived | The remaining 2026-07-03 audit plans are complete and removed; do not recreate or replay them. |
| 033 | DONE | Strict capability manifest and pre-side-effect validation. |
| 034 | DONE | Compiler-cache backend seam: sccache, Kache canary, and off. |
| 035 | DONE | Canonical storage contract and fail-closed identity. |
| 036 | DONE | Capacity leases, reservations, admission, and reclaim. |
| 037 | DONE | Guarded cache GC and physical accounting. |
| 038 | DONE | Job environment defaults and precedence. |
| 039 | IN PROGRESS | Exact selected groups are live for all three organizations and Tailrocks runs eight org-scoped slots on signed-apt 0.1.105. Sequential ChainArgos and jackin-project migration/smoke remains. |
| 040 | DONE | Current-V2 `services:` parity. |
| 041 | DONE | Fixture V-A proof, including inline matrix, cache backends, services, and registry sync. |
| 042 | DONE | Approved estate adapters, Pages V2/OIDC, composites, attest rejection, and trust gates. |
| 043 | DONE | Async lifecycle finalization, pre-create, and JIT overlap. |
| 044 | DONE | Trust-scoped git mirror and reflink checkout. |
| 045 | DONE | Timing records, summaries, and doctor SLO output. |
| 046 | DONE | `velnor-tools audit-ci` and lane comparison. |
| 047 | IN PROGRESS | java-monorepo PR #1753 head `3e2d21b5` includes canonical weekly `both`; fresh ChainArgos V-B/V-C and operator review remain. |
| 048 | IN PROGRESS | blockchain-nodes PR #651 head `8296582f` includes canonical weekly `both`; fresh ChainArgos V-B/V-C and operator review remain. |
| 049 | IN PROGRESS | Native v4 attestation is apt-deployed as 0.1.102 and reaches repository upload. GitHub rejects the mandated public Rekor V2 bundle because it has zero verifier-counted integrated timestamps; exact Sigstore RFC3161 TSA expansion approval, fixture proof, and Jackin Preview V-B/V-C remain. |
| 050 | DONE | Velnor dogfood three-lane proof, 33-second clean rerun, and release recovery drill. PR #100 was open and green at baseline; its current delivery state must be reconciled. |
| 051 | IN PROGRESS | Holla PR #37 head `94fc643a` is Velnor-green and includes canonical weekly `both`; manual V-B/V-C and operator review remain. |
| 052 | IN PROGRESS | Ruxel PR #3 head `3d7684fa` is Velnor-green and includes canonical weekly `both`; manual V-B/V-C and operator review remain. |
| 053 | IN PROGRESS | Parallax direct-main delivery through `7fe3f136` includes canonical weekly `both`; exact-head Velnor run `29854619258` is fully green after structural browser save/readiness fixes. Manual V-B/V-C and the Darwin-release decision remain. |
| 054 | IN PROGRESS | TermRock direct-main delivery through `deeefef8` includes canonical weekly `both` and a root-fixed public-API freshness gate. Exact-head Docs/Pages run `29849259484` attempt 4 is green after Velnor 0.1.105 fixed the required single-tar Pages artifact contract; manual V-B/V-C remains. |
| 055 | IN PROGRESS | Schemalane/pg-bigdecimal/tracing PR heads `fa090cc2`/`c1ac5c8c`/`84e6caeb` include canonical weekly `both` and are Velnor-green; manual V-B/V-C and operator review remain. |
| 056 | IN PROGRESS | Parallax telemetry playground is delivered directly through `b6ec71e9`; automatic Velnor run `29847627753` is green and manual V-B/V-C remains. |
| 057 | IN PROGRESS | TableRock trunk `31bb6b76` includes canonical weekly `both`; existing native and local proofs remain green, with manual applicable V-B/V-C pending. |
| 058 | IN PROGRESS at baseline | Phase-4 audit, performance report, and direction-doc reconciliation were executed; finish status/bookkeeping and any currently authorized delivery work. Human-only policy decisions remain excluded. |
| 059 | DONE | Recorded host baseline cleanup and post-cleanup smoke. |
| 060 | IN PROGRESS | Active program heads are migrated for nextest-only testing; former doctest coverage has nextest-discoverable integration/trybuild replacements. Velnor `1bd00cf` extends mechanical enforcement to live scripts, configuration, instructions, and Rust documentation; the current 13-repository audit has zero errors and zero test-runner findings. Fixture PR #4, estate delivery, and V-A/V-B/V-C remain. |
| 061 | IN PROGRESS | Produce one final reviewable delivery per estate repository, with local-only sccache, canonical weekly `both` parity, approved required checks, and exact PR/evidence ledger. Never merge the final PRs; stop only for operator review decisions. |

## Unblocking mandate

Treat every baseline `BLOCKED` row as work to resolve, not as a terminal label.
For each one, reproduce or revalidate the blocker, identify the architectural
or policy root cause, select the narrowest complete correction, implement it
when authorized, and prove the result. “Out of the old plan's scope” is not by
itself a reason to stop: this goal explicitly authorizes the exact
cross-repository source, Dockerfile, package-source, formatting, test, workflow,
and delivery-policy corrections necessary to finish items 047–057, provided
they preserve the fixture contract and do not add unrelated product work.

Use these resolution tracks:

| Item | Required autonomous resolution track |
|---|---|
| 039 | Re-check current credentials and org runner-group state. If still unauthorized, finish every non-admin migration prerequisite, validate the runbook against current GitHub behavior, determine the minimum exact permission and group/repository settings, and produce one executable operator command/checklist. Apply the migration immediately if ordinary authenticated access now permits it; never retry unchanged 403s. |
| 047 | Refresh java-monorepo PR #1753 and its checks, resolve safe drift, and leave the final merge-ready PR with exact required-check and reviewer state. Do not merge; operator review is the terminal boundary. |
| 048 | Diagnose the three package-build failures from current GitHub and Velnor logs. Fix their shared project-side root causes in blockchain-nodes—Dockerfiles and package sources are now in scope—add regression checks, then repeat V-B/V-C from the existing program branch. A lane-neutral failure must receive one shared fix, never lane-specific YAML. |
| 049 | First write the mandatory capability proposal for `actions/attest-build-provenance@v4`: why Jackin needs it; exact accepted ref, inputs, values, combinations, outputs, failure modes, Sigstore/OIDC/GitHub API behavior, trust/network/storage implications, upstream `actions/runner` and action-source evidence, and fixture proof. Do not implement this new security/network capability until the operator explicitly approves that described surface. After approval, implement the latest native Rust path, manifest validation, fixture coverage, and Jackin Preview V-B/V-C. Advance every non-dependent Jackin task while approval is pending. |
| 053 | Reproduce Parallax against current exact heads, compare official-runner and Velnor timing/state evidence, and perform a root-cause investigation of the loading-skeleton and CRUD race. Fix the approved browser/job-runtime behavior in Velnor if it diverges; otherwise fix the shared deterministic application/test race in Parallax, now in scope. Add a regression that fails on the old race, then repeat V-B/V-C without weakening assertions or goldens. |
| 054 | Fix the exact pre-existing Termrock rustfmt drift, verify no semantic change, refresh PR #4, and repeat the minimum lane/performance proof. |
| 057 | Re-read current Tablerock instructions and reconcile the estate delivery with its trunk-only rule. Preserve newer coverage; port only missing standard requirements through the repository's permitted delivery mechanism. Do not recreate the obsolete destructive plan diff or force a forbidden branch/PR. Prove the final trunk state with the applicable audit and lane gates. |
| 058 | Close only after the newly unblocked deliveries are reflected in the estate audit, campaign report, docs, plan index, and required-check handoff. Replace old exclusions with new evidence; retain only blockers that genuinely require authority unavailable to the executor. |

For every bug fix, record why the architecture or test design permitted the
class of failure and make the regression cover that enabling condition. Do not
apply retries, sleeps, golden updates, skipped steps, relaxed assertions, or
lane-specific conditionals as symptom workarounds.

## Estate configuration convergence (binding)

The desired end state is **identical implementation for every common or
required concern**, not identical repositories. Do not copy irrelevant jobs
into a repository merely for visual symmetry, and do not permit two approaches
to the same CI concern.

Before editing workflows, create or refresh a machine-readable estate concern
inventory consumed by `velnor-tools audit-ci`. For every repository, classify
each concern as `required`, `applicable`, `non-applicable`, or `repo-specific`,
with a short evidence field. Classification comes from the repository class in
`VELNOR_PROJECTS_SETUP.md`, its publish/deploy surfaces, and current workflows;
an absent workflow is not evidence that the concern is non-applicable.

| Concern | When required/applicable | Canonical implementation that must match |
|---|---|---|
| Lane selection | Every executable standardized workflow | Exact `lanes` choice input, default/options text, inline matrix, `runs-on`, lane display suffix, writer flag, and selected-lane aggregate behavior from §2.1. |
| Checkout | Every job needing repository files | Same pinned checkout major/SHA, fetch-depth rule, credential persistence rule, and ordering. Only the justified fetch depth/path parameters may vary. |
| Tool setup | Every job needing tools | mise, Rust toolchain, mold, and compiler-cache setup in the canonical order with identical pins/inputs/env; tool lists and versions are repository data in committed mise/toolchain files. |
| Rust CI | Every Rust repository/class | Canonical formatting, lint, audit, compile/test jobs and `ci-required` aggregation. Workspace/package commands may vary only where the repository layout requires it. |
| Integration/services | Repositories whose tests require services | Canonical lane matrix, service declaration, health behavior, tool/cache setup, artifacts, and aggregator; service image/name/ports and test command are parameters. |
| Cargo/cache | Every compiling Rust job | Exact registry/git, mutually exclusive compiler backend, target policy, key shape, env, setup/post reporting, and warm-run assertions from §2.4. |
| Docker/build | Repositories building images | Same pinned login/QEMU/Buildx/Bake/build-push steps, cache mode, provenance rule, writer gating, and artifact/result flow; image names, contexts, platforms, and registry credentials are parameters. |
| Artifacts | Every producer/consumer pair | Same current v4 upload/download adapter refs, retention/input rules, naming convention, and Results Service behavior; artifact payload paths/names are parameters. |
| Docs/Pages | Repositories building or deploying docs | Canonical build, Pages upload/deploy, environment/writer gate, concurrency, and required aggregator; docs command and site path are parameters. |
| Preview | Repositories producing previews | Canonical build/attest/publish flow, read-only secondary lane, retention, and required aggregator; target/package data may vary. |
| Release | Repositories publishing tags/packages/images | Canonical trigger, immutable pins, build reuse, attestation where approved, single-writer publish, recovery path, and required aggregator; package targets and credentials are parameters. |
| Renovate | Repositories running repository-local Renovate | Canonical filename, schedule/dispatch, pin, permissions, concurrency, timeout, and writer behavior; config path is a parameter. |
| Workflow safety | Every workflow | Canonical concurrency/cancel policy, timeout tier, least permissions, Ubuntu pin, no ad-hoc installers, no deprecated commands, and no lane conditionals except writer gate/name suffix. |

For each canonical concern:

1. Select exactly one exemplar/template after comparing the current estate to
   §2 and §9. The newest contract-conformant implementation wins; do not
   blindly copy a stale repository.
2. Express repository differences as explicit data parameters. If two YAML
   blocks differ structurally, either prove the product concern differs or
   converge them.
3. Add a missing required concern from the canonical template. Record and test
   `non-applicable`; never use an empty/skipped placeholder job.
4. Update the shared `.github/AGENTS.md` block identically everywhere, with
   repository-specific additions below a stable delimiter.
5. Extend `velnor-tools audit-ci` in Rust so it reports and fails on both
   `missing-required` and `canonical-drift`, while allowing documented
   `non-applicable` and `repo-specific` entries. Add fixture/unit tests proving
   all four classifications and preventing an omitted concern from passing.
6. Run the estate audit against program branches, fix every error, then run it
   against delivered default branches. Completion requires zero unexplained
   errors and a generated comparison report showing the canonical signature of
   every applicable concern in every repository.

Do not call configurations identical based only on job names. Equivalence must
cover behaviorally relevant YAML: triggers, permissions, lanes, runner, step
order, action refs and inputs, env, cache keys, timeouts, concurrency,
conditions, outputs, artifacts, and aggregation. Values explicitly classified
as repository data may differ; the mechanism consuming them must be identical.

Maintain a machine-checkable requirement/evidence matrix for the duration of
the goal. Each row identifies: plan/item, repository, concern, authoritative
requirement, applicability, implementation path/commit, direct test, workflow
run or artifact, delivery state, and status. Status is one of
`proven-complete`, `contradicted`, `incomplete`, `weak-evidence`, or `missing`;
only `proven-complete` may satisfy done. Missing mappings, stale paths,
duplicate concern ownership, or evidence that proves only a narrower claim are
work, not documentation footnotes.

The live matrix is `docs/program-requirement-evidence.tsv`; its concern-level
companion is `config/estate-repositories.json`. Update both with current
evidence as implementation, delivery, and external state change.

If implementation or an audit discovers an approved in-scope requirement with
no owning plan, create the next monotonic numbered plan, link its dependencies,
add it to `plans/README.md`, give it exact verification/evidence gates, and
execute it. Plans are the execution graph, not a ceiling that allows uncovered
requirements to disappear.

## Objective

Reach the complete program state, resolving as much as safely possible without
operator interaction:

1. Reconcile the baseline ledger against current local and remote state. A
   previously blocked item may become actionable because a PR was merged, a
   permission was granted, a target repository changed, or an approved fix
   landed. Verify; do not assume.
2. Complete safe, already-authorized delivery and bookkeeping work. In
   particular, reconcile Velnor PR #100, plan 058, `plans/README.md`, the
   remaining plan files, `plans/OPERATOR-REPORT.md`, and the direction docs.
3. Eliminate each blocker through the resolution tracks above. `BLOCKED` is a
   temporary checkpoint, never goal completion. When the sole next step needs
   unavailable external authority—coordinated destructive-history consent,
   org/repository administration, protected-branch approval, credentials, or
   approval of a fully described new strict capability—finish all preparation,
   request the exact action, keep the goal active, and resume verification as
   soon as the state changes. Project defects and old plan scope are not valid
   blockers.
4. Remove completed plan files from `plans/`; history retains them. Keep the
   one active prompt and only genuinely open plan files. Do not remove a plan
   until its outcome and evidence are captured in `plans/README.md` and, when
   operationally relevant, `plans/OPERATOR-REPORT.md`.
5. Finish with docs, prompt, plan index, current PR state, and actual delivery
   state mutually consistent. Do not describe blocked work as merged or done.
6. Complete plan 060: no executable or instructional `cargo test` remains in
   Velnor, the fixture, or any estate repository. Ordinary tests use
   `cargo nextest run`; former doctest coverage is preserved through
   nextest-discoverable regression tests because stable nextest does not run
   rustdoc doctests.

## Authorization boundaries

- Do not implement or admit a new action, input, value, backend, credential
  flow, trust surface, network call, or runtime behavior without explicit
  operator approval. This especially forbids silently adding
  `actions/attest-build-provenance@v4` support for plan 049.
- Do not change fixture or workflow semantics to conceal a Velnor failure.
  Fix an already-approved Velnor capability at its architectural root; request
  approval before expanding the declared surface.
- This goal authorizes the exact application, package-source, Dockerfile,
  formatting, test, and workflow corrections named in the resolution tracks.
  Keep each correction minimal, root-cause based, tested, and isolated from
  unrelated product changes.
- Do not change branch protection, required checks, org runner groups, remote
  compiler-cache trust, or scheduled parity policy without explicit authority.
- Do not bypass a target repository's `AGENTS.md`. If current instructions
  conflict with an old plan, current repository instructions win and the plan
  must be reconciled.
- Any Sentry installation or upgrade must follow commit + push + tag +
  `release-deb.yml` + signed `velnor-apt` publication + `apt-get update &&
  apt-get install`. Never deploy a local package or binary directly.
- Never delete ambiguous or non-Velnor host resources. Resolve exact ownership
  first and use only plan-approved, recoverable or narrowly targeted actions.

## Execution sequence

### 1. Establish current truth

- Read all sources above and every remaining plan file fully.
- Run `git status --short`, `git branch --show-current`,
  `git rev-parse --short HEAD`, and inspect commits since `a0a4ee5`.
- Query current states for Velnor PR #100, java-monorepo PR #1753, Jackin PR
  #810, Parallax PR #21, Termrock PR #4, and any PR named by remaining plans.
- Check whether the org permission, strict-capability
  approval, required-check decisions, or target-repository instructions have
  changed. Absence of evidence is not approval.
- Build a private working checklist mapping every item 014–059 to
  the requirement-matrix statuses above, with direct evidence. Persist the
  matrix in the repository when it becomes completion evidence rather than
  transient investigation state.

### 2. Eliminate technical and delivery blockers

- Reconcile historical PR #100 against its already-delivered state; do not
  replay or alter that completed delivery.
- Execute the item-specific resolution tracks above in dependency order.
  Technical blockers 048, 053, 054, and the policy-compatible work for 057 are
  active implementation tasks, not handoff notes.
- Re-check other open estate PRs and resolve safe merge drift. Never merge the
  final review set; leave exact required-check and reviewer state for the
  operator.
- After each fix, rerun focused local regression gates first, then only the
  stale V-A/V-B/V-C evidence required for that changed surface. Do not replay
  the full campaign without a concrete stale-evidence reason.
- When running any fixture or workflow verification, cancel prior active runs,
  delete exact stale runner registrations, dispatch one new run, and monitor
  only its ID. Check progress at least every 60 seconds and diagnose any state
  unchanged for two minutes.

### 3. Converge common and required configuration

- Build the concern inventory and canonical signatures before broad workflow
  edits. Review every repository, not only currently blocked repositories.
- Reconcile common concerns to the canonical implementations above and add
  missing required concerns using the same templates. Preserve genuinely
  repository-specific work as parameters or explicitly classified jobs.
- Update `velnor-tools audit-ci`, its Rust tests, fixture snippets, and the
  estate configuration so convergence remains mechanically enforced.
- Commit per repository in reviewable concern-based commits on its permitted
  delivery branch; then run static gates followed by V-A when a shared pattern
  changed and V-B/V-C for affected repositories.

For each smallest reviewable checkpoint: read the owning requirements and live
implementation; run drift/prerequisite checks; diagnose the root cause of each
failure class; implement the complete behavior; add tests proportional to the
surface; run focused then repository-wide gates; inspect output and artifacts;
update the requirement matrix, docs, plan state, and evidence; review the diff
for secrets, stale paths, bypasses, accidental divergence, and unrelated
changes; then deliver and verify remote state before advancing. A command
exiting zero proves only the behavior it actually exercises.

### 4. Complete Phase 4 reconciliation

- Treat `docs/ci-performance-report-2026-07-19.md` as the immutable baseline
  campaign report. As blockers close, collect the missing rounds and write a
  dated completion report (or clearly marked follow-up section) that replaces
  every old exclusion with passing evidence; never rewrite historical facts.
- Run the current estate audit only if all configured local clones exist and
  the run is needed to validate changed bookkeeping. A nonzero result is
  acceptable only when every error maps to an explicitly blocked delivery;
  never claim a delivered-default audit for repositories whose final PRs remain
  open; audit their exact delivery heads and label that evidence accurately.
- Mark plan 058 terminal only when its audit, campaign report, concern
  convergence, docs reconciliation, and human-policy decisions are implemented
  and accurately recorded. A prepared handoff is progress, not completion;
  keep the goal active until required administrative state is verified live.
- Update direction docs first if reconciliation changes the stated plan, then
  record the direction-log entry in `AGENTS.md`, then reconcile
  `plans/README.md` and this prompt. Do not invent a direction change merely
  to close bookkeeping.

### 5. Verify and clean the plan library

- Ensure `plans/README.md` contains only current truth and distinguishes
  completed implementation, blocked delivery, and human-only follow-up.
- Retain individual files only for genuinely open executable or
  approval-gated work. Remove stale completed plan files after their evidence
  is safely summarized. Plan 015 is complete and its individual plan file is
  retired; its backup checksum and post-rewrite evidence remain in the index
  and operator report.
- Keep `plans/OPERATOR-REPORT.md` append-only. Add only new decisions,
  deviations, and fresh evidence; do not rewrite its incident history.
- Verify all edited Markdown references and ensure no prompt references removed
  files as executable inputs.

## Verification gates

For Rust/code changes in this repository, all must pass:

```text
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo nextest run --workspace --locked
actionlint
```

For docs/prompt-only reconciliation, use focused read-only checks instead:

- `git diff --check`
- `rg` for stale `TODO`, `IN PROGRESS`, removed plan paths, old prompt-system
  references, and contradictory completion claims
- inspect `git diff -- plans/ docs/ AGENTS.md VELNOR_PROJECTS_SETUP.md`
- verify every cited PR/run/commit identifier exists and supports the claim

Do not run expensive or stateful verification without a changed surface or a
specific stale-evidence reason.

## Progress, persistence, and blocked work

Continue autonomously while safe in-scope work exists. Parallelize independent
read-only checks and advance bookkeeping while CI runs. Never wait blindly.

Do not turn missing authority into implied permission, but do not use it to stop
the whole goal. Finish every independent task, prepare the exact change or
operator action, request the missing decision once when it is the sole blocker,
and continue elsewhere. If no independent work remains, report the precise
pending authority/state and wait for it without marking the goal complete; on
resume, re-check the external state and continue from the recorded checkpoint.
Repeatedly retrying an unchanged 403 or branch policy is not progress.
Cross-repository defects are explicitly in scope and must be fixed.

Never stop, close, or declare the goal achieved because work is difficult,
long-running, awaiting CI, temporarily blocked, or dependent on an operator.
Keep the goal active across turns and context compaction. Use
`plans/README.md` plus the append-only operator report as the durable checkpoint
so work resumes without replay. The only successful terminal condition is the
fully verified definition of done below.

## Definition of done

- Every item 001–059 is accounted for, and every open item from 014–059 is
  reconciled against current evidence; no completed item
  is rerun and no blocked item is presented as complete.
- Every technical blocker is root-caused, fixed, regression-tested, and
  re-verified; no row remains `TODO`, `IN PROGRESS`, or `BLOCKED`.
- Plan 015's coordinated history rewrite is complete across every reachable
  ref, force-push/reclone coordination is recorded, and post-rewrite scanning
  proves the rendered HTML capture absent without exposing token values.
- Plan 039's organization runner groups and repository access are live with the
  documented permission model, followed by a successful registration and
  smoke proof; a prepared runbook alone is insufficient.
- Plans 047–057 have one final reviewable PR or binding trunk-only delivery
  record per repository, with required V-B/V-C evidence green. Final PRs remain
  open for operator review and are never merged by automation.
- Every repository has a reviewed concern classification; every required
  concern exists; every common/applicable concern matches its canonical
  behavioral signature; every non-applicable or repo-specific classification
  has evidence; and `velnor-tools audit-ci` reports zero unexplained
  `missing-required` or `canonical-drift` findings on delivered default
  branches.
- Shared workflow filenames, job ids, lane plumbing, action pins/inputs, step
  order, environment, cache keys, timeouts, concurrency, writer gates,
  artifacts, and aggregators are identical wherever the concern applies.
  Repository-specific commands, paths, packages, targets, and secrets differ
  only as explicit data parameters.
- Plan 049's attestation capability is implemented only after explicit approval
  of its complete proposal, then native-Rust, manifest, fixture, and Jackin
  Preview proofs all pass.
- Plan 058 is terminal, with the 2026-07-19 campaign report and estate-audit
  exclusions represented truthfully.
- `plans/README.md`, remaining plan files, this prompt,
  `plans/OPERATOR-REPORT.md`, direction docs, and current PR states agree.
- Completed plan files are retired; only genuinely open work remains.
- The worktree passes the checks appropriate to the changed surface.
- The final report lists: files changed, verification run, final PRs and
  trunk-only records, items completed, and confirmation that no approval boundary was
  crossed. If any program PR is still open or any operator action is still
  required, this definition is not met and the goal remains active.
- Before declaring completion, perform two consecutive independent full audits
  of the live requirement/evidence matrix, all 13 delivered default branches,
  the fixture, Velnor, current CI/artifacts, plan statuses, and direction docs.
  Each audit must discover zero new requirement, applicability, canonical-drift,
  missing-required, evidence, delivery, or documentation gap. Record both audit
  results separately. Any discovery resets the count after it is fixed.
