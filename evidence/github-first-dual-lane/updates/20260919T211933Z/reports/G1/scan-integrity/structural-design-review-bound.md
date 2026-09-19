# G1 scan-integrity bound-design review

Decision: **CONDITIONAL APPROVE the design direction; implementation may be
prepared only after the bounded API contract correction below.** This review
does not approve source, generated output, or the G1 gate. No source, runtime,
Docker, or host operation occurred.

## Exact input

- Commit: `a7690262a5ec5258ec7c1832f248665d126fdbf7`, branch
  `codex/g0-estate-scope`, clean worktree `/private/tmp/g0-estate-scope`.
- Commit tree: `4f422872937d91fa0cf65f099535f79f08cb4032`.
- Tracked artifact: `plans/g1-scan-integrity/structural-design.md`, 499 lines /
  30,135 bytes, SHA-256
  `cf2df95c6bbf6857248b9bddd23a46adb1af59020ff2d88021c0b119d93e371f`.
- Commit includes the actual artifact; it is no longer pointer-only.

## Prior blocker disposition

| Prior blocker | Result | Evidence |
| --- | --- | --- |
| Protected producer identity | Resolved in design | Immutable `ProtectedPolicyContract`, caller App/installation proof, validator workflow/revision/closure, exact checks, enforcement, bypass actors (`:60-90`); API comparison and failure states (`:95-137`); wrong-App fixture (`:435-449`). |
| External generator/source closure | Resolved in design | Separate generator repository identity and commit/tree/blob acquisition, closure and byte checks, fail-closed mismatch (`:114-128`, `:441-449`). |
| Local lifecycle / no-baseline downgrade | Resolved in design | Explicit mutually-exclusive `--baseline-revision` / `--local-no-baseline`; protected mode cannot select local `none`; protected failures are terminal before rendering (`:139-170`, `:287-296`, `:420-425`). |
| Current-renderer self-authority | Resolved in design | Mode-bound `CurrentRenderContract`, `CurrentOnlyUnbound`, protected path/content binding, hostile new-path fixture (`:190-220`, `:413-419`). |
| Detector ordering/fixed point | Resolved in design | Mandatory bounded three-pass protocol, exact controls, cycle error/no-write behavior (`:222-271`, `:458-465`). |
| Force/stale/config-drift semantics | Resolved in design | Exact class table, force contract, integrated SI-P2–P4 fixtures (`:213-220`, `:287-350`, `:378-398`). |
| Atomic apply ambiguity | Resolved in design | Same-filesystem journal, preimage rechecks, rollback/error state, injected-failure fixture (`:352-368`, `:466-470`). |

## Readiness matrix

| Area | Verdict | Notes |
| --- | --- | --- |
| Sidecar/header/digest trust | Pass | Claims-only boundary remains explicit (`:33-56`, `179-182`). |
| Protected baseline provenance | Pass, pending API correction | Caller contract is concrete and cannot be selected by target config/worktree (`:60-90`). API paths, raw response retention, external source closure, and fail-closed behavior are specified (`:95-137`). |
| Operator baseline usability | Pass | Full SHA object mode, explicit local preview mode, object-db hardening, diagnostics, no mutation on unavailable baseline (`:139-182`). |
| Current-only/new-path safety | Pass | Candidate-only paths are `CurrentOnlyUnbound`, visible/conflicting, and force-immune (`:190-220`, `413-419`). |
| Raw versus detector inputs | Pass | Raw audit retained; only authenticated exact outputs are removed; controls cannot be extended by config/sidecar (`:222-271`). |
| Config drift/stale removal/force | Pass | Required ordering and integrated positive/negative fixtures are concrete (`:287-350`, `378-398`). |
| Races, symlinks, transaction rollback | Pass in design | Preimage/identity checks and bounded rollback protocol are stated (`:320-368`, `426-470`). Implementation must prove them on one frozen SHA. |
| Legacy compatibility | Pass | Old broad unowned replacement is removed; no aliases/fallbacks (`:172-177`, `342-344`). |

## Required bounded correction before implementation

### API schema and permission contract

The design currently says the protected contract and API verification use exact
`(context, app_id)` pairs for both branch protection and rulesets (`:65-74`,
`105-113`). GitHub's primary REST schemas are not identical:

- branch protection exposes required-check `checks[].app_id`;
- repository rulesets expose required-status-check `parameters.required_status_checks[].integration_id`, which is optional, not an `app_id`.

See [GitHub branch-protection REST schema](https://docs.github.com/en/rest/branches/branch-protection)
and [repository-ruleset REST schema](https://docs.github.com/en/rest/repos/rules),
which documents `integration_id` for ruleset checks.

Do not silently equate the fields. Amend the plan to either:

1. carry source-specific pairs (`(context, app_id)` versus
   `(context, integration_id)`) and compare each against the immutable caller
   contract; or
2. define and verify an independent App/integration mapping before normalizing.

The fixture must cover null/missing IDs and a wrong integration ID, not only a
wrong branch-protection App ID.

The exact-bypass requirement also has a caller-permission prerequisite:
GitHub documents that `bypass_actors` on a repository ruleset is returned only
to callers with write access to that ruleset. The current base policy workflow
declares only `contents: read` (`.github/workflows/ci-policy.yml:31-32`) and
uses the workflow token for API acquisition (`:64-70`). Before implementation,
the upstream policy caller must provide either a narrowly scoped trusted App/
attestation that can read the required ruleset fields or an explicit
fail-closed “protected baseline unavailable” deployment prerequisite. The
generator must never treat an omitted `bypass_actors` field as an empty set.

See [GitHub repository-ruleset access note](https://docs.github.com/en/rest/repos/rules#get-a-repository-ruleset).

This is a real feasibility condition, not scope expansion: without the field,
the plan's required exact bypass proof cannot execute.

### Small consistency fix

The required-ordering conflict list (`:309-314`) names
`ModifiedGenerated` and `ForeignOrUnknown` but omits the newly introduced
`CurrentOnlyUnbound` class (`:218`). Add it explicitly so the implementation
cannot interpret the new hostile-path class as merely diagnostic. The class
table and SI-N2b already require conflict/no-write; this is a wording
consistency fix, not a new behavior.

## Implementation gate

After the two bounded corrections above, implementation can proceed on a fresh
immutable source SHA. Required proof remains the design's SI-B1–B4, hostile
matrix, full library suite, snapshot, fmt/diff/clippy, and rollback tests. Do
not claim G1 approval from this design or from helper-only tests.

