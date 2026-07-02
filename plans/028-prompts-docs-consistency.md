# Plan 028: Reconcile prompts/checklists and dated docs with the master-plan's in-production state

> **Executor instructions**: Follow step by step. This is a **docs-only** plan;
> verify claims against code before editing. STOP ⇒ report. Update
> `plans/README.md` when done.
>
> **Drift check (run first)**:
> `git diff --stat 1645ccc..HEAD -- prompts/ docs/`
> If these changed, re-read the current state before editing.

## Status

- **Priority**: P3
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: docs
- **Planned at**: commit `1645ccc`, 2026-07-03

## Why this matters

AGENTS.md's flagship rule: prompts/README must never contradict `docs/`; if they
disagree, `docs/` wins and the prompt must be fixed. Several prompts and dated
docs now contradict the master-plan's "achieved and in production" status, which
has a concrete cost: a `/goal` run on a stale prompt re-grinds already-shipped
verification work, and dated "follow-up" lists re-open closed items.

## Current state (verify each before editing)

- `prompts/README.md:21-24` says the four-prompt sequence is "complete ...
  coverage, UI parity, fixture proof, and ChainArgos parity all shipped," yet
  still presents the full run-sequence table + `/goal` block
  (`prompts/README.md:32-52`) and instructs working each checklist
  "top-to-bottom ... stops when everything is checked and green"
  (`prompts/README.md:80`).
- `prompts/target-workflow-coverage.checklist.md` = 0/58 checked and
  `prompts/chainargos-runner-parity.checklist.md` = 0/85 checked, while
  `docs/vision.md:14-16` and `docs/master-plan.md:46-49` state this work is in
  production.
- `prompts/chainargos-migration-outstanding.checklist.md` has `[ ]` items that
  other docs mark DONE (e.g. line ~45 "apt-native repo + .deb", line ~78
  "auto-publish on tag" vs master-plan P1.6 DONE; line ~94 "Stop idle-exit churn"
  vs master-plan P1.2 DONE). It is also an orphan: a `.checklist.md` with no
  paired `.md` and not in the README run-sequence table (violates
  `prompts/README.md:60-72`).
- Dated docs contradicted by 0.1.22–0.1.29 commits:
  - `docs/stability-gap-audit-2026-06-11.md:26-29` lists per-cycle docker prune
    and container memory caps as "not yet implemented," but both shipped
    (`runner.rs:641` `prune_stale_velnor_docker_resources`; 0.1.28 `533a5c4`
    "cap job containers"; `debian/velnor-daemon.service:19-20`
    `VELNOR_JOB_CPUS`/`VELNOR_JOB_MEMORY`).
  - `docs/master-plan.md` P1.8 says the Homebrew job is "stubbed `if: false`,"
    but `.github/workflows/release.yml:149-150` is now a bare comment
    placeholder (no `if: false` job).

**Before editing, re-verify each claim against the current code/commits** (the
repo is at a later version than these docs; a claim may have changed again).

## Commands you will need

| Purpose        | Command                                                          | Expected            |
|----------------|------------------------------------------------------------------|---------------------|
| Verify a claim | `git log --oneline -- <path>` / `rg <symbol> crates/`           | confirm shipped/not |
| Checklist scan | `rg -c '^\- \[ \]' prompts/*.checklist.md`                       | unchecked counts    |

## Scope

**In scope**:
- `prompts/README.md`, the two shipped prompts + their checklists
  (`target-workflow-coverage`, `chainargos-runner-parity`),
  `prompts/chainargos-migration-outstanding.checklist.md`.
- The two dated docs' follow-up lists + master-plan P1.8 wording.

**Out of scope**:
- `docs/vision.md`, `docs/mission.md`, `docs/roadmap.md`, the master-plan's core
  sequence — these are the source of truth; do not rewrite them (only fix the
  P1.8 Homebrew phrasing).
- Deleting genuinely-open items — reclassify them, don't drop real work.

## Git workflow

- Branch: `advisor/028-prompts-docs-consistency`
- Commit: `docs: reconcile prompts/checklists and dated docs with in-production status`
- Do NOT push/PR unless instructed.

## Steps

### Step 1: Mark the shipped prompts superseded

Add a clear "Superseded — this goal is shipped; see `docs/master-plan.md`
Phase <N>" banner at the top of `target-workflow-coverage.md` and
`chainargos-runner-parity.md` (and their checklists), so a `/goal` run does not
re-grind 58/85 already-passing boxes. Reconcile `prompts/README.md`: either move
these out of the active run-sequence table or annotate them as complete so the
table matches the "sequence is complete" statement at lines 21-24.

**Verify**: `prompts/README.md` no longer presents shipped prompts as active
work; the shipped prompts carry a superseded banner linking the master-plan.

### Step 2: Reconcile the migration checklist

For `chainargos-migration-outstanding.checklist.md`: re-verify each `[ ]` item
against code/commits. Check off (or annotate "shipped in <version>") the ones
that are done (apt repo/.deb, auto-publish on tag, idle-exit churn). Fold any
**genuinely open** items (e.g. log-archive, `Run <command>` step names, buildx
local cache — re-verify these are still open) into the appropriate master-plan
phase, then either give this checklist a paired goal file + README table row, or
archive it. Do not leave it an orphan contradicting other docs.

**Verify**: no item in this checklist is `[ ]` while the code shows it shipped;
the file is either in the README run sequence or archived.

### Step 3: Caveat the dated docs

Add a one-line "as-of <version>; later status in `docs/master-plan.md`" caveat to
the follow-up lists in `stability-gap-audit-2026-06-11.md`, and mark
docker-prune + mem-caps as shipped (with the `runner.rs:641` / 0.1.28 pointer).
Correct master-plan P1.8's Homebrew wording from "stubbed `if: false`" to
"commented placeholder" (matching `release.yml:149-150`).

**Verify**: the two dated follow-up items are marked shipped; P1.8 wording
matches `release.yml`.

## Test plan

- No code tests. Each edited claim must be backed by a `git log`/`rg` check that
  you cite in the PR description.

## Done criteria

- [ ] Shipped prompts carry a superseded banner; `prompts/README.md` no longer lists them as active work
- [ ] `chainargos-migration-outstanding.checklist.md` has no `[ ]` item that the code shows shipped; it is in the README sequence or archived (no orphan)
- [ ] Dated docs' stale follow-ups are marked shipped/caveated; master-plan P1.8 wording matches `release.yml`
- [ ] Every changed claim is backed by a cited `git log`/`rg` check in the PR
- [ ] Only files under `prompts/` and `docs/` modified (`git status`)
- [ ] `plans/README.md` row updated

## STOP conditions

- Re-verification shows a claim is **not** actually shipped (the code differs
  from what this plan assumed) — do not mark it done; report the discrepancy.
- An item's status is genuinely ambiguous — leave it `[ ]` with a note rather
  than guessing.

## Maintenance notes

- This is a snapshot reconciliation; the AGENTS.md hard rule (docs win; fix
  prompts on divergence) is the ongoing process. Consider a CI check that flags a
  prompt claiming active work the master-plan marks done (a follow-up).
- Reviewer: spot-check two or three "marked shipped" items against the cited
  commits.
