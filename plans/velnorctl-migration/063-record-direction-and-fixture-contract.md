# Plan 063: Record the velnorctl direction and fixture control contract

> **Executor instructions**: Complete this plan before product implementation.
> Update both repositories through normal reviewed changes. Do not weaken any
> existing fixture. If the fixture baseline cannot pass unchanged, diagnose and
> fix Velnor first; do not hide the failure in the new workflow.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- AGENTS.md .github/AGENTS.md README.md docs/vision.md docs/roadmap.md docs/prompt.md plans/README.md crates/velnor-tools/src/main.rs scripts`
> Compare live text with the evidence below before editing.
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

Refreshed 2026-08-24 at Velnor `aed09eb`; the original planning-time gaps in
steps 1–2 are already closed on the campaign branch, and the citations below
are live anchors:

- `docs/vision.md:70-83` records the authoritative direction: performance,
  native adapters, and UX stay active, and the velnorctl migration names the
  final crate layout (`velnorctl`, service-only `daemon` entrypoint,
  `velnor-model`/`velnor-control`/`velnor-client`/`velnor-render`,
  maintainer-only `velnor-tools`), zero backward-compatible aliases, complete
  `velnor-runner` removal after Plan 079, and Debian apt/dpkg as the sole
  installed-version authority with no release command family or activation API.
- `docs/prompt.md:19-24` scopes the active surface to exactly the marked
  unified-CI contract, the tracked goal graph, and the `plans/TASKS.md`
  mirror; no earlier prompt or plan is active.
- `.github/AGENTS.md:5-10` instructs the sole `lane=github|velnor|both`
  selector with organization-derived defaults and no universal Velnor default;
  lines 15–17 separate runner group `velnor-trusted` from selection label
  `velnor-target-mvp`; lines 21–24 name final `velnorctl` binary/package
  ownership with `velnor-runner` interim until Plan 079.
- `crates/velnor-tools/src/main.rs:18` defaults fixture tooling to
  `tailrocks/velnor-actions-fixture`.
- Fixture `compat.yml` covers real execution semantics. It lacks deterministic
  hold/fail/cancel phases needed by `logs`, `wait`, lifecycle, queue, and event
  validation. Its current manual input is `lanes`, which conflicts with the
  marked contract's sole selector `lane`.

## Drift reconciliation 2026-08-24

Reconciled at Velnor b57b036 against live repository state:

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
- `plans/README.md` now contains the `velnorctl-migration` category row and
  already orders Plan 063 first; no index reconciliation is required here.

### Execution reconciliation 2026-08-24 (at Velnor aed09eb)

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

### Execution closeout 2026-08-24 (at Velnor 82b15f5)

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

**Status**: BLOCKED — queue-isolation proof requires operator-provided
dedicated validation runner labeled `velnor-cp-queue-validation`; all other
criteria evidenced.

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

## Steps

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

## Test plan

- Unit tests for corrected default fixture slug and dispatch input validation.
- Fixture static tests for every scenario and lane.
- Live both-lane compatibility run plus all eight control scenarios.

## Done criteria

- [x] Direction files and prompt agree on complete `velnor-runner` removal.
- [x] Root and `.github` instructions agree on sole `lane` selector, runner
      group/label distinction, and class-derived defaults.
- [x] Direction files agree that apt/dpkg exclusively own installed-version
      management and no Velnor release command/resource/API remains.
- [x] Fixture tooling defaults to `tailrocks/velnor-actions-fixture`.
- [x] Control workflow adds coverage without changing existing fixtures.
- [x] `rtk mise run check` passes in both repositories.
- [x] Fresh fixture run IDs and conclusions are recorded.

## STOP conditions

- Existing fixture is not green before the new workflow is added.
- Canonical fixture-class generation still emits `lanes` or cannot produce the
  byte-identical `lane` contract.
- New fixture needs an unapproved capability or weakens an existing assertion.
- Direction documents disagree about final binary/package ownership.

## Maintenance notes

Later tasks may extend control scenarios, but must preserve success, failure,
hold, and cancellation semantics. Workflow dispatch cleanup rules remain
mandatory for every migration task.
