# /goal prompt — Execute the Velnor estate standardization program (plans 033–059)

Pass everything below this line to `/goal`, verbatim.

---

Execute the complete Velnor estate standardization program to the end: every
plan from `plans/033-*.md` through `plans/059-*.md` in this repository
(`/Users/donbeave/Projects/tailrocks/velnor-project/velnor`), ordered and
gated by `plans/README.md` section "Program 033–058" (which includes 059).
Work fully autonomously. Never ask me anything. Never stop to wait for my
input. Keep delivering until every plan row in `plans/README.md` is DONE, or
BLOCKED with recorded evidence after every documented fallback was exhausted.

## Source of truth, in priority order

1. `AGENTS.md` hard rules (fixture-is-contract; ≤2-minute wait cycles;
   cancel stale runs and delete stale runner registrations before
   dispatching; latest protocol/version always; actions/runner is the
   protocol source of truth; strict capability contract; log-format
   contract; docs/ is direction truth).
2. The individual plan files `plans/033`–`plans/059` — read each one fully
   before starting it; follow its steps, verification commands, and scope
   boundaries exactly.
3. `VELNOR_PROJECTS_SETUP.md` (the law: §2.0 portability + freshness laws,
   §2.1–§2.12 standard incl. uniform shape, §2.11 budgets, §8 delivery
   model and V-A→V-D gates).
4. `docs/strict-capability-contract.md` and
   `docs/storage-and-disk-pressure-2026-07-18.md` for the runner-side
   requirements.

## Delivery model (binding)

- Exactly one branch per repository carries that repo's entire program work:
  `velnor-estate-standard` — including this velnor repo (plans 033–046, 050,
  058 land there as one commit series) and the fixture repo (plan 041).
- **Sole exception — jackin (plan 049): do not create a new branch.**
  Resolve the head branch of https://github.com/jackin-project/jackin/pull/810
  (`gh pr view 810 --repo jackin-project/jackin --json headRefName`), check
  it out, merge latest `main` into it if behind, and stack the ENTIRE jackin
  delivery on top of it. PR #810 stays jackin's single program PR; where the
  plan's changes touch files #810 already modified, integrate toward the
  union of #810's clarity/reuse direction and the estate standard — they are
  the same program.
- Every commit: conventional message, DCO signoff (`git commit -s`).
- Push each program branch and open its PR (except jackin — PR #810 already
  exists; push to its branch and update its description) once the repo's
  static gates pass. PR descriptions carry what the plans mandate (timing
  tables, verification run URLs, deviations).
- Merge a repo's PR yourself when ALL its gates are green (V-B three-lane
  parity, V-C budgets, actionlint, repo test gates) and branch protection
  permits. If protection blocks you, record it in
  `plans/OPERATOR-REPORT.md` and move on — never sit waiting.

## Autonomy rules — never ask, never block

- Resolve every ambiguity yourself from the priority order above. If two
  sources conflict, higher priority wins; record the conflict and your
  resolution in `plans/OPERATOR-REPORT.md`.
- Plan STOP conditions are re-interpreted for this run as follows:
  - **Safety STOPs** (ambiguous ownership on the live host, anything that
    would weaken the fixture or a workflow to mask a Velnor gap, secret
    exposure, deleting non-velnor data): do NOT do the dangerous thing.
    Skip that single item, record it with evidence in
    `plans/OPERATOR-REPORT.md`, and continue with everything else. Never
    guess on live-server deletions — plan 059's owned-resource-only rule is
    absolute.
  - **Progress STOPs** (a parser rejects the canonical expression, upstream
    behavior differs from the plan's assumption, a dependency is missing):
    apply the plan's documented fallback if it has one; otherwise implement
    the minimal contract-conformant alternative, mark the deviation in the
    plan's status row and PR description, and keep going. Exception: if
    GitHub rejects the canonical inline lane matrix in the fixture (plan
    041), that invalidates the standard's §2.1 — design the closest working
    single-expression form, prove it in the fixture on both lanes, use it
    estate-wide consistently, and document the substitution prominently.
- A failing Velnor lane is ALWAYS fixed in the runner, never by changing the
  workflow's semantics. That runner fix is in scope even when it exceeds the
  current plan — do it, then continue.
- Never idle: while any CI run, fleet operation, or long build is in flight,
  advance another plan. Maintain the wave order from `plans/README.md`
  (Wave A runner P0 → Wave B estate Phase 1 → Wave C runner P1 + Phase 2 →
  Wave D Phase 3 + close), parallelizing independent plans.
- Monitoring discipline (AGENTS hard rule): check state at least every 60
  seconds during active waits; never wait more than 2 minutes blind; a job
  pending >2 minutes means investigate (stale runner, wrong label, fleet
  down) and heal, not wait.
- Before every workflow dispatch: cancel pending/in-progress runs from
  earlier attempts and delete stale runner registrations, then dispatch and
  monitor ONLY the new run id.

## Verification (non-negotiable)

- Gate V-A: plan 041's fixture proof lands and passes on BOTH lanes before
  any estate repo work is dispatched.
- Per repo: dispatch `lanes=velnor`, `lanes=github`, `lanes=both` from the
  program branch; all green; identical job/step sets across lanes; record
  the three run URLs. Then the §2.11 timing pass (cold / warm / no-change
  rerun, both lanes) with the table in the PR description; the Velnor
  no-change rerun must show zero dependency downloads/compiles and zero
  tool installs.
- Plan 059: run the read-only inventory immediately; run the destructive
  pass only after plans 035+037 land (or immediately before the first
  campaign window), strictly within its owned-resource guardrails; commit
  the baseline report. Every timing claim references that baseline.
- Runner code gates on every velnor commit series:
  `cargo fmt --all --check`, `cargo clippy --workspace --all-targets
  --locked -- -D warnings`, `cargo nextest run --workspace --locked`,
  `actionlint`.

## Bookkeeping

- Update the matching status row in `plans/README.md` the moment a plan
  starts (IN PROGRESS) and finishes (DONE / BLOCKED + one-line reason).
- Maintain `plans/OPERATOR-REPORT.md` as an append-only log of: decisions
  you made in my place, deviations from plan text, skipped safety items,
  and anything that genuinely needs a human (credentials, org admin
  settings, branch protections) — with exact commands or clicks I must run.
  That file replaces asking me questions.
- Keep `docs/` and `AGENTS.md` consistent with anything you change
  (direction-log entries for direction changes), and finish with plan 058's
  docs reconcile and estate campaign report.

## Definition of done

Every row 033–059 in `plans/README.md` is DONE (or BLOCKED with exhausted
fallbacks and evidence); all 13 estate repos plus velnor and the fixture are
green on all three lanes from their single program branch (jackin = PR
#810); the host baseline report, the estate performance campaign report, and
the final docs reconcile are committed; `plans/OPERATOR-REPORT.md` lists the
few human-only items, if any. Do not declare done before that state is
real — verify it, then state it plainly.
