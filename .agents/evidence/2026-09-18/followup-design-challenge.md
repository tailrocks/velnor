# Design challenge: Q1 / Q2 / Q2b followup proposals (2026-09-17)

READ-ONLY challenge to `plans/2026-09-17-migration-open-questions-decisions.md`.
No repo files touched. Evidence: velnor2 worktree + `jackin-dev.yml` /
`reuse-compliance.yml` at `origin/pr/994` in the jackin checkout.

Common finding: all three proposals generalize the *plumbing dimension named in
the question* (a field, a pin) while leaving the harder content dimensions
(triggers, concurrency, job graph, policy bodies) unexamined. Each verdict
below is REJECT-as-specified with a smallest coherent alternative.

---

## Q1: `events` on `[[check_profile]]`, XOR with `schedule`, shared set per file

### (1) Genuinely generic or consumer-shaped?

Mechanism generic, placement and rule consumer-shaped. `events` as GitHub
event names is generic vocabulary. But the XOR-with-schedule plus
"all rows share the set" rule is fitted to exactly one consumer (reuse:
push/PR/dispatch, no cron) and copies `check_shared_cadence`
(`primitives/check_profiles.rs:113-129`) without its justification. The
shared-cadence rule exists because mixing crons over-executes every job on
every cron (`check_profiles.rs:8-10`). Mixing event sets has no such hazard:
a file CAN carry push+PR+dispatch+schedule at once — `docs_site` does exactly
that (`primitives/docs_site.rs:209-219`, fixed events + optional schedule).
The proposal is stricter than the platform requires and stricter than the
precedent it cites ("Precedent: docs_site already renders event-split gates"
— but docs_site never XORs; triggers there are primitive-owned, not
per-row-declared). No second consumer demonstrates the per-row XOR shape.

### (2) Already supported elsewhere?

Partially, in three places the decision does not mention:

- `workflow_dispatch` already renders on EVERY scheduled-checks file
  (`check_profiles.rs:231-233`). "No dispatch" was never true; only push/PR
  triggers and cron-lessness are missing.
- `docs_site.render_triggers` is the in-tree proof that fixed event triggers
  + optional schedule compose. The established pattern is "triggers owned by
  the primitive/file", not "events declared per profile row".
- CI units already run product commands on PR/push inside the aggregate.
  The doc never states why reuse needs a standalone file rather than a unit
  (own status context? dispatch lanes? push-main runs?). Standalone status
  granularity is a plausible answer, but it must be written down — otherwise
  the followup builds a second PR-lint mechanism without justifying it
  against the first.

### (3) Preserves necessary behavior?

No — underspecified on three behaviors where the consumer file differs from
what the renderer hardcodes (comparing `reuse-compliance.yml` at pr/994
against `render_scheduled_checks`, `check_profiles.rs:212-243`):

1. **Concurrency.** Renderer hardcodes `cancel-in-progress: true`
   (`check_profiles.rs:238`). Both consumer files use PR-only-cancel
   (`cancel-in-progress: ${{ github.event_name == 'pull_request' }}`). An
   event-triggered file under today's renderer cancels superseded MAIN runs.
   For a lint gate that may be acceptable — but it is a behavior change vs
   the PR copy and the decision never mentions it.
2. **Lanes.** Consumer has a `lanes` dispatch input driving a
   velnor/github/both runner matrix. Profiles have a single `runner`
   (`config/mod.rs:300`, `profile_runs_on`). The rendered reuse file would be
   single-lane. Which lane? Product decision required, or lanes
   generalization — currently silent.
3. **Either/or identity.** Exactly-once coverage (`primitives/mod.rs:1334-1364`)
   + XOR means a profile can never be both scheduled AND evented. The decision
   bakes "checks are either/or" into config validation without stating it,
   while `docs_site` (schedule AND events) contradicts it.

### (4) Removes the bug class or papers over it?

Papers over, narrowly. The bug class is "trigger gaps force hand-maintained
workflow copies" (the #994 disease). XOR-events cures it for the one consumer
but rebuilds the wall one step out: the next consumer wanting schedule+events
(a shape `docs_site` already proves legitimate) hits the XOR error and
hand-maintains again. A fix whose core rule the codebase's own precedent
violates is not structural.

### (5) Smallest coherent alternative

File-level `events` on the `scheduled-checks` DECLARE row args (next to
`profiles`/`name`, `check_profiles.rs:38-40`), not a `[[check_profile]]`
field. `schedule` stays per-profile; the renderer emits the shared-cron
trigger (as today) PLUS the declared event set. Sharing is then automatic
(one row = one set): no cross-row validation, no config-level XOR, no new
per-row field. Cron-lessness falls out by allowing schedule-less profiles
only in files whose declare row sets `events` — validated in
`select_profiles`, where file context already exists and coherence is already
enforced (`check_profiles.rs:65-109`). Even smaller variant: no arg at all —
fixed push/PR/dispatch on the primitive like `docs_site`, triggers owned by
generic code, profile rows untouched. Per-row `events` constrained to be
identical across rows is the worst of both: declared, yet forbidden to vary.

### Verdict: REJECT as specified

Required corrections:

- C1. Move `events` from the `[[check_profile]]` row to the declare-row
  args (file-level), or fix the set on the primitive. No per-row field, no
  XOR in `validate_check_profile_row`.
- C2. Decide evented-file concurrency explicitly: PR-only-cancel expression
  (matching both PR consumer files and `docs_site.rs:607`) vs hardcoded
  `true`. Do not inherit always-cancel silently.
- C3. Decide lanes explicitly: single-lane evented profiles (Jackin declares
  which lane) or a lanes-matrix generalization. The consumer file has a
  matrix; the renderer has none.
- C4. Allow schedule+events composition (docs_site shape); forbid only what
  over-executes. State the rule as "one file, one trigger set", not
  "one profile, one trigger kind".
- C5. Write the "why not a CI unit" justification into the followup slice
  plan. If no good answer exists, reuse becomes a unit and Q1 evaporates.

---

## Q2: release family renders to the row's declared `file`

### (1) Genuinely generic or consumer-shaped?

Plumbing generic, proposal consumer-shaped by omission. Removing the
canonical pin (`primitives/mod.rs:1321-1332`, `Release::render` hardcoding
`"release.yml"`, `release.rs:148-153`) is generic plumbing. But the `release`
schema (`release.rs:124-146`) has contract args only — no `name`, no
triggers, no paths, no concurrency, no lanes, no tasks. With per-row files
and per-row contracts, two `release` rows render two files each containing
`name: Release`, `on: push: tags: ["v*"]`, `concurrency: release-${{github.ref}}`
(`release.rs:2591`, `2503`, `3120`). Identical names, identical tag triggers,
identical concurrency groups across two files: duplicate workflow names,
double-publish races on the same tag, indistinguishable runs. The decision's
"Per-row contract args already exist" is true and irrelevant — contracts were
never the blocking dimension. The only consumer, jackin-dev, matches NO
existing renderer output (push-main+PR+dispatch triggers, PR-only-cancel
concurrency, version/version/assert/build/publish job graph, manifest-bound
version). So the proposal generalizes the pin for one unexamined consumer
shape while leaving every content dimension pinned.

### (2) Already supported elsewhere?

The SHAPE jackin-dev needs overlaps existing machinery the decision never
evaluates:

- The `preview` family already renders a rolling main-branch publisher:
  push-main+dispatch triggers, rolling publish, producer/mode bindings
  (`release.rs:1722`, `2172`, bindings `1729+`). jackin-dev
  (main-branch publisher, no tags) is closer to preview-with-matrix-and-gate
  than to tag-triggered release. The decision never compares against it.
- Target-matrix machinery already exists (`release_matrix_runner`,
  `release.rs:1839`; E "matrix completeness").
- Multi-trigger files already exist (`maintenance`: PR-closed + schedule +
  dispatch, `release.rs:3377-3381`).
- Modes already resolve rolling vs stable, PR vs push (`resolve-mode`,
  `runtime.rs:2846+`, tests `4959-5046`).

File-pin removal is new; but "second publisher shape" is a mapping problem
across existing kinds, not a pin problem. The followup must map jackin-dev
onto kinds first.

### (3) Preserves necessary behavior?

File generalization alone preserves nothing of jackin-dev: wrong name
(`Release` vs `jackin-dev`), wrong triggers (tags vs main/PR/dispatch),
wrong concurrency (per-ref no-cancel vs workflow+ref PR-cancel), wrong job
graph (verify-tag + run-full-CI vs version-gate + version + assert + build +
publish), tag-bound version vs manifest-bound version. Rendering jackin-dev
through `render_release` today would produce a different workflow that
double-publishes on tags. The evidence section correctly identifies the pin
but mistakes the pin for the gap. Also unexamined: `RELEASE_SIDE_FILES`
default rows (`release.rs:29-34` — "every entry the generated surface owns
gets a default row, unless the config declares the family"). What renders the
default `release.yml` when two `release` rows exist? If the default row still
renders, the duplication triples. The default-row interaction must be
specified before any pin removal.

### (4) Removes the bug class or papers over it?

Papers over — and regresses the failure mode. The bug class is "one family =
one file = one shape forces product workflows into hand-maintained copies".
Removing the pin without parameterizing content removes the ERROR but not the
cause: the next multi-shape consumer still cannot render (content hardcoded),
so copies persist — but the failure moves from a loud fail-closed usage error
("must declare `release.yml`") to silently wrong workflow bytes. Fail-closed
becomes fail-open-bytes. That is worse than the status quo, not a step past it.

### (5) Smallest coherent alternative

Name the second SHAPE first, then unpin the file for the family that owns it:

- Keep the canonical pin for `release`: a tag-triggered stable publisher is
  plausibly one-per-repo BY CORRECTNESS (two publishers on `v*` is
  incoherent — the pin may be a guardrail, not a gap; the decision never
  considers this).
- Express jackin-dev as what it is — a rolling main-branch binary
  publisher — via the `preview` family (which already owns push-main triggers
  + rolling publish + producer/mode bindings) extended with per-row file and
  the needed matrix/version-gate jobs; or a new rolling-publisher kind if
  preview's rolling-prerelease semantics do not fit.
- Parameterize `name`, triggers, and concurrency per row as part of the same
  slice — file generalization without content generalization is dead plumbing.

File generalization without a second shape is unusable; a second shape without
file generalization is inexpressible. The decision does the second half first
and declares victory.

### Verdict: REJECT as specified

Required corrections:

- C6. Name jackin-dev's publisher SHAPE before touching the pin: rolling
  main-branch binary publisher; decide preview-kind vs new kind with a
  trigger/job-graph diff against both renderers.
- C7. Generalize `name`, triggers, and concurrency per row in the same slice
  (schema + renderer + validation). Two files with identical
  name/triggers/concurrency must be a usage error, not renderable output.
- C8. Specify the `RELEASE_SIDE_FILES` default-row interaction: what the
  default `release.yml` row does when the family has N declared rows (mine:
  declaring the family suppresses the default row entirely, for all N).
- C9. Justify or keep the `release` pin: if tag-triggered stable publishing
  is one-per-repo by correctness, the pin stays and jackin-dev was never a
  `release` row. Do not unpin what should be a guardrail.

---

## Q2b: version-policy jobs rendered from release-family args (CHALLENGED HARDEST)

Proposal: "version-policy jobs rendered from release-family args (generic
policy, product-owned version source)".

### (1) Genuinely generic or consumer-shaped?

Consumer-shaped. Disassemble the actual policy body (`jackin-dev.yml`,
`validate-version-bump` + `version` + `assert-version`):

- artifact path set: `Cargo.toml`, `crates/jackin-dev/**` minus `*.md`,
  `Cargo.lock` — product paths, product exclusion rule (orientation
  docs/symlinks don't change the artifact);
- offline `cargo tree -p jackin-dev --locked --edges normal,build` closure
  compare with sed normalization, via a detached worktree at base SHA —
  product package, product edge set, product normalization;
- `version = "..."` sed extraction from a product manifest path, base vs head
  compare;
- `assert-version`: `gh release view` reuse check + Homebrew formula curl
  against `jackin-project/homebrew-tap` — product tap, product formula,
  product reuse policy.

A "generic version-policy renderer" parameterized over all of this is a
product workflow with extra steps — the parameters ARE the product. The
decision's split ("generic policy, product-owned version source") leaves the
closure check, path set, exclusions, and formula check homeless with no
account of where they go. No second consumer exists; the parameterization
surface exceeds the policy. This is the precise shape the project's
genericity law forbids: "the named tasks own every product assertion and
threshold, so generic code never interprets a check result"
(`config/mod.rs:290-293`, `check_profiles.rs:4-7`).

### (2) Already supported elsewhere?

Yes — three homes, and the decision misreads all three:

1. **Classification already exists as selection, not enforcement.**
   `version_bump_units` + `version_bump_matches` (`runtime.rs:1528-1595`)
   already implement version-bump classification generically (allowlisted
   units, manifest+lock diff, version-line-only check) — but as CI SELECTION
   narrowing (`runtime.rs:1435-1455`), not a failing gate. The decision's
   "only records unit names into project.toml, enforcing nothing" is true of
   enforcement and misses that any generic gate must reuse or explicitly
   supersede this classifier, not duplicate it. Note also that
   `version_bump_matches` is itself cargo- and layout-specific (`crates/`
   prefix, `rust-` unit ids, `runtime.rs:1553-1562`) — a warning, not a
   template: a second copy doubles the consumer-shaped surface.
2. **Modes already resolve PR → validate.** `resolve-mode` maps
   `pull_request`/`schedule` to validate (`runtime.rs` tests `5022-5032`;
   "producer runs publish, dispatches drill, and everything else validates").
   The modes machinery already HAS the validate slot for PR events. The
   missing piece is a validate-mode job that runs product checks — not a
   policy renderer.
3. **The genericity law already answers this, in the same doc.** Q1's
   decision: "REUSE lint stays a named task; generic code owns triggers
   only." Q2b proposes the opposite split (generic policy body) for version
   enforcement with zero justification for the inconsistency. Same project,
   same doc, opposite boundary — one of them is wrong, and the weight of
   in-tree precedent (check_profile tasks, docs_site commands, unit commands)
   is unanimously behind named tasks.

Related ledger correction: L8 claims "version_bump_units + release kinds
cover version gates". Selection narrowing is not a gate: it chooses which
units run, it never fails a PR for a missing bump. The ledger entry is false
for enforcement and needs correction; the decisions doc is right to flag the
gap, wrong about the home.

### (3) Preserves necessary behavior?

A generic renderer cannot preserve the policy without absorbing the product:

- Omit the cargo-tree closure check → lockfile-only dependency-closure
  changes slip through un-bumped. That is behavior LOSS vs the PR copy: the
  exact case `jackin-dev.yml`'s worktree/tree-compare guards. Silent
  under-enforcement on a release gate is the worst outcome on offer.
- Absorb it → the "generic" renderer takes package name, edge set, offline
  toolchain needs, sed normalization, manifest path/syntax, tap URL: a
  consumer-shaped primitive wearing generic syntax.
- `assert-version` is two behaviors, not one: published-result reuse (overlaps
  generic admission machinery — `image-admission`/`verify` jobs — must be
  mapped or explicitly dropped) and formula-version agreement (pure product).
  The decision mentions neither.

### (4) Removes the bug class or papers over it?

Moves the bug class across the boundary and creates a worse one. "Version
drift ships unbumped artifacts" is enforced TODAY by product shell. Moving
the BODY into generic code does not remove a bug class — it relocates product
logic to the party that cannot verify it, creating "generic policy
misclassifies a product it doesn't understand": wrong path set, wrong
closure, wrong version file → false pass ships a bad release, or false fail
blocks PRs. Fail-closed usage errors cannot save this: correctness depends on
product facts the generator cannot check. The structural fix is the boundary
the project already chose everywhere else: generic owns the GATE (job
placement, PR-only `if`, `fetch-depth: 0` requirement, failure propagation),
product owns the CLASSIFICATION (named task, tested by the product). That
removes "no gate exists" while keeping "wrong classification" with the party
that can test it. The proposal does the reverse on both counts.

### (5) Smallest coherent alternative — and why it wins

Generic version-GATE job, product-owned named task. The rolling-publisher
file renders a PR-only `validate-version` job — checkout with `fetch-depth:
0`, then `mise run <declared-task>` — where the task name is a declared arg
(the exact shape of check_profile `tasks`). The task body lives in Jackin; it
can BE the extracted jackin-dev shell, tested by Jackin. Gate it on
mode==validate where modes exist (resolve-mode already maps PR→validate, so
no new event logic). Generic code gains: one job renderer + one string arg +
validation (task declared, PR-only placement, full-history checkout). No
Cargo parsing, no paths args, no closure args, no formula args in generic
code. If a SECOND consumer later needs the same SHAPE (PR-only named-task
gate in a publisher file), the job renderer is already generic; only then —
with two consumers and a proven-common body — consider lifting shared bodies.
That is the project's own generalization discipline; Q2b as specified skips it.

### Verdict: REJECT the generic policy renderer. Version enforcement stays a product-owned named task behind the generic gate.

Required corrections:

- C10. Split the gate from the policy: generic renders the PR-only
  `validate-version` job (placement, `if`, `fetch-depth: 0`, failure
  propagation); Jackin owns the task body (paths, closure check, version
  compare, formula check) as a named task. Mirror the Q1 boundary from the
  same doc ("generic code owns triggers only") or justify the divergence in
  writing — divergence requires evidence, not assertion.
- C11. Reuse `resolve-mode`'s PR→validate mapping for gate placement; do not
  invent new event→policy dispatch. If modes are absent on the file, the
  PR-only `if` is the gate.
- C12. Supersede-or-reuse `version_bump_matches`: either the gate's
  classifier replaces the selection-narrowing copy (one classifier, used
  twice) or the followup states why selection-classification and
  gate-classification must differ. Two cargo-specific classifiers is a
  refusal-grade outcome.
- C13. Account for `assert-version` separately: map published-result reuse
  onto generic admission machinery or explicitly drop it with reason; keep
  the formula check product-owned. It is not part of "version policy" and
  must not ride along inside a policy renderer.
- C14. Correct ledger L8 ("version_bump_units + release kinds cover version
  gates"): selection ≠ enforcement. The ledger currently asserts coverage
  that does not exist, which is how this gap survived to a followup.

---

## Required design corrections (index)

| # | Proposal | Correction |
|---|---|---|
| C1 | Q1 | File-level `events` (declare-row arg) or primitive-fixed set; no per-row field, no XOR |
| C2 | Q1 | Explicit evented-file concurrency decision (PR-only-cancel vs `true`) |
| C3 | Q1 | Explicit lanes decision (single lane declared, or matrix generalization) |
| C4 | Q1 | Allow schedule+events composition; rule is "one file, one trigger set" |
| C5 | Q1 | Written "why not a CI unit" justification, or reuse becomes a unit |
| C6 | Q2 | Name the second publisher SHAPE first (preview-kind vs new kind) with renderer diffs |
| C7 | Q2 | Per-row `name`/triggers/concurrency in the same slice; duplicate identity is a usage error |
| C8 | Q2 | Specify default-row suppression when the family has N declared rows |
| C9 | Q2 | Justify removing the `release` pin, or keep it as a one-stable-publisher guardrail |
| C10 | Q2b | Generic gate job + product-owned named task; no generic policy body |
| C11 | Q2b | Gate placement via `resolve-mode` PR→validate, no new event dispatch |
| C12 | Q2b | One version classifier: supersede-or-reuse `version_bump_matches` |
| C13 | Q2b | `assert-version` mapped/dropped separately; formula check stays product-owned |
| C14 | Q2b | Correct ledger L8: selection is not enforcement |

## One-line summary

Q1 puts file-level data on the wrong row and XORs what the precedent composes;
Q2 unpins the filename while every content dimension stays pinned (fail-closed
→ fail-open-bytes); Q2b inverts the project's own genericity law by moving a
product policy body into generic code. All three are reject-as-specified; the
alternatives above are smaller, precedent-backed, and keep each bug class with
the party that can test it.
