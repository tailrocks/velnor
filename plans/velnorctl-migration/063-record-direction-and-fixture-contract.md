# Plan 063: Record the velnorctl direction and fixture control contract

> **Historical record notice (Plan status: DONE):** This plan records completed
> historical work only. It is historical and non-authoritative; do not execute
> its procedure or derive current selector requirements from it. Current
> execution follows the active repository contract and current coordination
> instructions.
>
> Reconciled 2026-08-24 at Velnor b57b036; generator ce23409 emits sole `lane`; fixture pin dc4204c green (compat run 32675488430).

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: MED
- **Depends on**: none
- **Category**: direction, tests
- **Planned at**: Velnor `35d5bb7`; fixture `dc4204ca055c3138cf78d666b4dd1c5adfddc963`; 2026-08-24

## Why this matters

Repository policy requires direction documents, `AGENTS.md`, plan index, and
active prompt to agree before execution. Existing fixture proves workflow
compatibility but does not deliberately hold, fail, or cancel a job for control
plane inspection. Every later task needs one stable integration corpus.

## Current state

Historical snapshot refreshed 2026-08-24 at Velnor `aed09eb`; the original
planning-time gaps in steps 1–2 were closed during that historical campaign.
Current execution follows the operator-assigned branch/focused-PR topology
recorded in external `../COORDINATION.md`, anchored to 2026-08-31 at main SHA
`d413b1ef445ec9e6156dbdc1d92b491b5436f77a`; the citations below are live
anchors:

- `docs/vision.md:92-105` records the authoritative direction: performance,
  native adapters, and UX stay active, and the velnorctl migration names the
  final crate layout (`velnorctl`, service-only `daemon` entrypoint,
  `velnor-model`/`velnor-control`/`velnor-client`/`velnor-render`,
  maintainer-only `velnor-tools`), zero backward-compatible aliases, complete
  `velnor-runner` removal after Plan 079, and Debian apt/dpkg as the sole
  installed-version authority with no release command family or activation API.
- `docs/prompt.md:19-24` scopes the active surface to exactly the marked
  unified-CI contract, the tracked goal graph, and the `plans/TASKS.md`
  mirror; no earlier prompt or plan is active.
- `.github/AGENTS.md:5-10` records the canonical plural `lanes` choice
  (`velnor | github | both`) with organization-derived defaults; callable
  reusable workflows keep singular `lane`, derived by callers from
  `inputs.lanes`; lines 18–20 separate runner group `velnor-trusted` from
  selection label `velnor-target-mvp`; lines 24–27 name final `velnorctl`
  binary/package ownership with `velnor-runner` interim until Plan 079.
- `crates/velnor-tools/src/main.rs:20-21` defaults fixture tooling to
  `tailrocks/velnor-actions-fixture`.
- Fixture `tailrocks/velnor-actions-fixture` is pinned here for reproducibility:
  main commit `3a01651d12f1285f92a7cdd4ec9087433bdea10c` and
  `.github/workflows/compat.yml` blob `13f7aa6227c36f39498800e392cdb75508f11e0f`;
  `.github/workflows/control-plane.yml` blob
  `be95f5e4895579f4c5baa1238590a7f9440ea67d` (verified 2026-08-31). It covers
  real execution semantics but lacks deterministic
  hold/fail/cancel phases needed by `logs`, `wait`, lifecycle, queue, and event
  validation. Its current manual input is `lanes`, matching the canonical
  plural selector contract; callable workflows derive singular `lane` from
  `inputs.lanes`.

## Historical drift reconciliation 2026-08-24

Reconciled at Velnor b57b036 against live repository state:

> This dated reconciliation is historical and non-authoritative for current
> execution or selector requirements.

- The canonical fixture-class generator (`tailrocks/velnor-actions`) at
  committed state ce23409 already emits the sole `lane` input in all five class
  templates; that input-name change is committed. Its working tree carries
  unrelated uncommitted concurrency-isolation work owned outside this campaign;
  it must not be touched or relied on.
- Estate-wide selector propagation has partially landed:
  `ChainArgos/java-monorepo` main carries singular `lane`; `jackin`,
  `schemalane`, `velnor`, the fixture, and this repository's generated workflow
  bytes still reference plural `lanes`. That propagation is tracked for later
  phases and the Follow-up queue.
- The executable step-3 gate remains exactly as written below and is defined as:
  generator-emitted bytes applied to Velnor-tui workflows, fixture
  compat/control-plane aligned to sole `lane`, and live proof. Never a hand
  fork of `compat.yml`.
- Fixture pin dc4204c is clean and green (compat main run 32675488430).
- At that historical snapshot, `plans/README.md` contained the
  `velnorctl-migration` category row and ordered Plan 063 first. The current
  index state is maintained in `plans/README.md` and `plans/TASKS.md`.

### Historical execution reconciliation 2026-08-24 (at Velnor aed09eb)

Recorded during Plan 063 execution; supersedes the stale spans above where
they disagree:

- Direction anchors verified live at aed09eb: `docs/vision.md:70-83`,
  `docs/roadmap.md:42-49`, `AGENTS.md:168-177`, `.github/AGENTS.md:5-24`,
  `README.md:19-21`. Step 1 is verification-only at this HEAD.
- This repository's generated `.github/workflows/ci.yml` now consumes the
  singular `lane` input (aed09eb applied generator code-class bytes);
  `release.yml` and `renovate.yml` have no canonical generated counterpart at
  ce23409 and remain unchanged.
- Correction to the ce23409 claim above: all five class *ci.yml* templates
  emit the sole `lane` input, but `templates/apt/package-update.yml:9` and
  `templates/tap/package-update.yml:9` still dispatch plural `lanes`.
  STOP condition #2 therefore remains open until fixed in the external
  generator repository, whose dirty working tree on branch
  `fix/concurrency-event-name` is owned outside this campaign and must not be
  touched here.
- The fixture checkout (`tailrocks/velnor-actions-fixture`, main at dc4204c)
  is clean and green locally (`mise run check`: actionlint, l2-closure,
  nextest 7/7), satisfying STOP condition #1. Its `compat.yml:10` still
  consumes plural `lanes`,   so the byte-identical rollout required before
  adding the control workflow has not landed. Control-plane creation stays
  stopped per step 3 until both external gaps close; live dispatch evidence
  for step 4 remains gated behind it.

### Historical execution closeout 2026-08-24 (at Velnor 82b15f5)

**LANED**: docs direction verified at aed09eb; generator sole-lane fix landed in
`tailrocks/velnor-actions` branch `campaign/sole-lane-package-update` commit
`7296821` with gates green; fixture PR #90 squash-merged as
`799178c1b53ddb5d1db5ddfc46d41c6284e1b72b` (control-plane corpus + compat
sole-lane, all 13 checks green); live compat `lane=both` run `32703106587`
succeeded on both lanes; control scenarios at fixture main `799178c`:
success `32703865136`, failure-at-named-step `32704019420`,
hold-cancelled-no-orphans `32704204228`, concurrent-overlap-20s
`32704312250`, artifacts-3-distinct `32704438283`, cache cold `32704574052`
warm `32704719858`, load-teardown `32704848711`. Evidence:
`.velnor-compare/2026-08-24-control-plane/`.

**UNPROVEN EXACT**: queue isolation — run `32705006580` was cancelled after
>4 minutes queued; no runner carries label `velnor-cp-queue-validation`; no
JIT registration was observed; requires an operator-provided dedicated
validation instance. GitHub-leg control scenarios remain pending (defaults
dispatched the Velnor lane).

**Deferred reviewer follow-ups** (both diffs APPROVE): explicit
`CP_CACHE_STATE` marker; org-slug charset check `fleet_policy.rs:196`; audit
duplicate re-resolution `fleet_policy_client.rs:615`; fixture squash trailer
formatting; Velnor rendered `${{ steps.controlled-failure.outcome }}`
literally (expression-parity observation).

**Status**: DONE (2026-08-24). The queue-isolation gap is closed by validation
run r2 on dedicated runner `cp-queue-validation-154447` (registration 5540):
all eight control scenarios report `CP_VERDICT=match`. Full mapping in
[Historical evidence (2026-08-24)](#historical-evidence-2026-08-24) below.

### Supersession 2026-08-24 (operator ruling): plural lanes restored

The operator ruling of 2026-08-24 restored the plural `lanes` choice input
(`velnor | github | both`, Velnor default) as the canonical estate dispatch
selector; callable reusable workflows keep their singular `lane` input and
callers derive it from `inputs.lanes`. Authority:
`tailrocks/velnor-actions` origin/main `87b3c31..84a3d2c` (release
`2026.8.31`) and fixture PRs #85–#88. This repository realigned at `85df3ab`
("ci!: adopt canonical plural lanes selector estate-wide").

Consequences for the evidence above:

- The sole-lane adoption chain is HISTORICAL and superseded: `aed09eb` ci
  adoption, generator branch `campaign/sole-lane-package-update` commit
  `7296821`, and the fixture sole-lane merge (recorded as PR #90 /
  `799178c`; live main carries the same corpus as squash merge #91,
  `932d97d`). Any statement in this plan or in `.github/AGENTS.md` naming a
  sole dispatch selector describes that superseded state.
- The live gate is now the plural-`lanes` restoration on fixture
  `compat.yml` + `control-plane.yml`: fixture PR
  https://github.com/tailrocks/velnor-actions-fixture/pull/92 (branch
  `campaign/restore-lanes-corpus`, commit `106328f`; restores the exact #87
  input block, `inputs.lanes` derivation in all matrix expressions, all eight
  scenario semantics unchanged, and realigns the workflow-surface audit to
  the canonical selector). Callable pin remains `eeb8a18` on release
  `2026.8.31`. Gates: actionlint + workflow-surface audit + l2-closure +
  nextest 7/7 green; PR checks CLEAN/MERGEABLE. Not merged by this campaign.

**Status**: DONE (2026-08-24). Both blockers closed: the plural-`lanes`
restoration merged to fixture main as PR #93 squash `1661158`, and the
isolated queue proof landed in `.velnor-compare/plan063-r2-queue-validation/`
(8/8 `CP_VERDICT=match`). Full mapping in
[Historical evidence (2026-08-24)](#historical-evidence-2026-08-24) below.

## Scope

**Velnor repository**:

- `docs/vision.md`, `docs/roadmap.md`, `docs/prompt.md`, `AGENTS.md`,
  `.github/AGENTS.md`, and active root `README.md` references
- `plans/README.md`
- `crates/velnor-tools/src/main.rs` and its fixture-default tests
- fixture validation scripts only when needed to enforce the cleanup sequence

**Fixture repository**:

- `.github/workflows/control-plane.yml` (create)
- `README.md`
- fixture-local tests/audits that enumerate workflow surface

**Out of scope**: runner behavior, capability manifest changes, systemd,
packaging, removal of any existing fixture job.

## Steps (HISTORICAL / NON-AUTHORITATIVE)

> **Historical procedure notice (Plan status: DONE):** The procedure below is
> retained as execution evidence only. It is historical and non-authoritative;
> do not execute it or derive current selector requirements from it. Its
> singular-dispatch wording is superseded by the canonical plural `lanes`
> contract stated in the current criteria and evidence below.

### 1. Make the direction authoritative

Update `docs/vision.md` first, then `docs/roadmap.md`. State final crate layout,
the service-only `velnorctl daemon` entrypoint, command-by-command migration,
intentional lack of backward aliases, and final deletion criteria. Add a dated
direction-log entry to `AGENTS.md`. Reconcile `docs/prompt.md` and
`plans/README.md` to point at this plan set.

Record the package boundary explicitly: `velnorctl` has no release command
family or release resource. Debian package metadata is authoritative for the
installed version; signed apt is the only installation, upgrade, downgrade,
rollback, and recovery mechanism. Velnor must not maintain an active-version
symlink, previous-version pointer, duplicate version history, or package
activation API. Package build/sign/publish remains CI/maintainer work, not an
operator CLI surface.

Reconcile `.github/AGENTS.md` and active root README text in the same reviewed
direction change. They must name the sole `lane=github|velnor|both` selector,
the class-derived default, runner group `velnor-trusted`, runner selection label
`velnor-target-mvp`, and the final binary/package ownership. Do not leave
a lower instruction file that reintroduces `lanes` or a universal Velnor
default.

**Verify**: `rtk rg -n "velnorctl|velnor-runner|lanes|lane" docs/vision.md docs/roadmap.md docs/prompt.md AGENTS.md .github/AGENTS.md README.md plans/README.md` shows the new direction, sole selector, and no contradiction claiming `velnor-runner` remains final architecture.

### 2. Correct fixture ownership in maintainer tooling

Change the default slug and fixture-status tests from `donbeave/...` to
`tailrocks/velnor-actions-fixture`. Keep explicit `--repo` overrides.

**Verify**: `rtk cargo nextest run -p velnor-tools --locked` passes; no default
fixture reference points to `donbeave`.

### 3. Add a control-plane fixture without weakening compatibility coverage

Before adding the control workflow, make the canonical fixture-class generator
emit the sole `lane=github|velnor|both` input and apply its byte-identical output
through the unified-CI rollout. Never hand-fork `compat.yml` inside this
migration. Record the resulting exact fixture commit; every later plan pins
that commit. If the canonical generator rollout has not landed, STOP here.

Add a manual workflow using only already approved actions and bash. Required
inputs: `scenario=success|failure|hold|queue|concurrent|artifacts|cache|load`,
`hold_seconds` bounded to `0..300`, `artifact_count` bounded to `1..8`, and
`lane=github|velnor|both`. Each scenario has deterministic named steps and
machine-readable markers. `queue` targets a dedicated validation instance so
no other runner can claim it; `concurrent` holds two jobs; `artifacts` emits
multiple bounded sanitized artifacts; `cache` exposes cold/warm/unchanged
markers; `load` applies bounded CPU/memory/disk activity with ready marker,
safety ceilings, declared measurement tolerance, and full teardown. Jobs
deliberately fail only for `failure`. Add a hosted aggregator
that reports the requested terminal state. Never synthesize hostile archives
or disk pressure in GitHub; Plans 074/075 test those through isolated fake API
and storage roots.

**Verify**: fixture `mise run check` and `actionlint` pass; workflow audit proves
full SHA pins, concurrency, timeouts, `cargo nextest` policy, and canonical lane
selection.

### 4. Mandatory fixture integration: establish clean live baseline

Before dispatch, cancel all non-completed fixture runs. Delete only stale GitHub
runner registrations with the dedicated validation-name prefix; confirm no such
registration remains. Dispatch existing `compat.yml` with `lane=both`, capture
the new run ID, and inspect only it every at most 60 seconds. If queued or
unchanged for two minutes, inspect runner group/label, daemon registration, and
broker/registry logs immediately.

Then dispatch every `control-plane.yml` scenario. Cancel the hold run through
GitHub and prove GitHub reports `cancelled`. Prove the queue is isolated,
concurrent jobs overlap, artifacts are distinct/bounded, and unchanged cache
markers report expected reuse. Record run URLs and sanitized JSON only.

**Verify**: `compat` lane comparison passes; control success succeeds, controlled
failure fails at its named step with logs, and hold cancellation terminates with
no orphaned runner registration.

## Historical test plan

- Unit tests for corrected default fixture slug and dispatch input validation.
- Fixture static tests for every scenario and lane.
- Live both-lane compatibility run plus all eight control scenarios.

## Done criteria

- [x] Direction files and prompt agree on complete `velnor-runner` removal.
- [x] Root and `.github` instructions agree on the canonical plural `lanes`
      selector, callable singular `lane` derived from `inputs.lanes`, runner
      group/label distinction, and class-derived defaults.
- [x] Direction files agree that apt/dpkg exclusively own installed-version
      management and no Velnor release command/resource/API remains.
- [x] Fixture tooling defaults to `tailrocks/velnor-actions-fixture`.
- [x] Control workflow adds coverage without changing existing fixtures.
- [x] `rtk mise run check` passes in both repositories.
- [x] Fresh fixture run IDs and conclusions are recorded.

## STOP conditions

- Existing fixture is not green before the new workflow is added.
- Canonical fixture-class generation still emits a noncanonical selector rather
  than the byte-identical plural `lanes` contract.
- New fixture needs an unapproved capability or weakens an existing assertion.
- Direction documents disagree about final binary/package ownership.

## Maintenance notes

Later tasks may extend control scenarios, but must preserve success, failure,
hold, and cancellation semantics. Workflow dispatch cleanup rules remain
mandatory for every migration task.

## Historical evidence (2026-08-24)

> **Historical evidence notice:** The dated evidence below is historical and is
> not current execution authority.

Mapping of each done criterion to concrete, machine-verifiable pointers.
All selector statements below reflect the canonical plural `lanes` choice
input (`velnor | github | both`, Velnor default) per the operator ruling
recorded in `AGENTS.md` ("Plural `lanes` selector restored as canonical",
2026-08-24) and the unified-CI contract selector clause.

- **Direction files and prompt agree on complete `velnor-runner` removal** —
  `docs/vision.md:70-83` (final crate layout `velnorctl` / service-only
  `daemon` entrypoint / `velnor-model`-`velnor-control`-`velnor-client`-
  `velnor-render` libraries / maintainer-only `velnor-tools`; zero
  backward-compatible aliases; complete `velnor-runner` removal after
  Plan 079), `docs/roadmap.md:42-49`, `docs/prompt.md:19-24`, and the dated
  direction-log entry in `AGENTS.md` ("velnorctl migration adopted",
  2026-08-24).
- **Root and `.github` instructions agree on the canonical selector,
  runner group/label distinction, and org-derived defaults** —
  `.github/AGENTS.md:5-9` names the plural `lanes` choice input with exactly
  `velnor | github | both` and `inputs.lanes` derivation for callable
  reusable workflows; `.github/AGENTS.md:18-19` separates runner group
  `velnor-trusted` from selection label `velnor-target-mvp`; organization
  defaults follow the unified-CI contract (jackin-project/* → `github`,
  tailrocks/* and ChainArgos/* → `velnor`).
- **apt/dpkg exclusively own installed-version management** —
  `docs/vision.md:70-83` records Debian package metadata as the sole
  installed-version authority with no Velnor release command family or
  activation API; the `AGENTS.md` direction-log "velnorctl migration
  adopted" entry repeats it as law.
- **Fixture tooling defaults to `tailrocks/velnor-actions-fixture`** —
  `crates/velnor-tools/src/main.rs:18`.
- **Control workflow adds coverage without changing existing fixtures** —
  fixture `compat.yml` job set unchanged across dc4204c → `932d97d`
  (squash #91) → `1661158` (squash PR #93); `control-plane.yml` adds the
  eight scenarios (`success|failure|hold|queue|concurrent|artifacts|cache|
  load`) with no existing job removed or weakened.
- **`rtk mise run check` passes in both repositories** — VELNOR repository:
  exit 0, nextest 909/909 (2026-08-24). Fixture repository: PR #93 checks
  all SUCCESS ×11 including `ci-required` and `DCO`.
- **Fresh fixture run IDs and conclusions are recorded**:
  - r1 compatibility + control proof:
    `.velnor-compare/plan063-c5-c7-fixture-proof-20260824/SUMMARY.md`.
  - Deliberate hold-cancel run `32704204228` in
    `.velnor-compare/2026-08-24-control-plane/`: GitHub reports
    `cancelled`, no orphan runner registration remains.
  - r2 queue validation: `.velnor-compare/plan063-r2-queue-validation/` —
    8/8 scenarios `CP_VERDICT=match`, isolated queue runner
    `cp-queue-validation-154447` (registration 5540) claimed only the
    dedicated label, concurrent scenario overlap_seconds=20, artifacts
    count=3 distinct=3, cache cold/warm verdict match.

Closing notes: fixture main dispatch selector restored to plural `lanes`
via PR #93 squash merge `1661158`; the generator-side exemption marker
`GENERATOR_PENDING_SOLE_LANE` is documented in the fixture audit script
(`.github/scripts/audit_workflow_surface.py`) pending upstream propagation
to `tailrocks/velnor-actions`.
