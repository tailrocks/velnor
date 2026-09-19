# G1 scan-integrity final bounded-design review

Decision: **DESIGN READY FOR BOUNDED IMPLEMENTATION**, with protected-authority
integration explicitly unavailable until its upstream caller is supplied. This
is not G1/source approval. Local operator implementation may proceed with the
protected path fail-closed; G1 remains gated on a real trusted authority
fixture/integration and the later immutable source review.

## Exact input

- Commit: `68495325421f15fa46989a6fbae880ec0d289d6a`, branch
  `codex/g0-estate-scope`, clean worktree `/private/tmp/g0-estate-scope`.
- Commit tree: `1f1f49031569aa99ad5a707c7354a83dac273acc`.
- Tracked artifact: `plans/g1-scan-integrity/structural-design.md`, 533 lines /
  32,247 bytes, SHA-256
  `82b3d94b9de4d3b6b18953e5972e7aecd6aa315a3f78cfccedb40fe15c75109a`.
- Design is explicitly design-only (`:1-7`); no source/runtime/host/Docker
  operation occurred.

## Prior corrections verified

- Source-specific branch `(context, app_id)` and ruleset
  `(context, integration_id)` are separate in the contract and comparison;
  null/missing/ambiguous ruleset integration IDs fail closed (`:67-96`,
  `102-124`, `461-483`).
- Ruleset bypass visibility is now an explicit trusted authority. Omitted
  `bypass_actors`, 403/404, or absent authority yields
  `baseline_policy_unproven`, never `[]`; the generator cannot upgrade the PR
  token (`:125-140`, `463-483`).
- `CurrentOnlyUnbound` is in the required conflict/no-write list and force
  contract (`:336-371`).
- External generator acquisition, missing-pin behavior, object-db hardening,
  local lifecycle, bounded detector convergence, and rollback remain concrete
  (`:141-209`, `249-298`, `380-396`).

## Readiness matrix

| Area | Verdict | Evidence |
| --- | --- | --- |
| Sidecar/header/digest trust | Pass | Claims-only boundary (`:36-59`). |
| Protected baseline semantics | Pass as fail-closed design | Immutable caller contract, source-specific IDs, external generator closure, and terminal errors (`:61-165`). |
| Local/operator usability | Pass | Explicit full-SHA baseline or explicit local no-baseline; no implicit downgrade (`:166-209`, `314-378`). |
| Current renderer authority | Pass | Mode-bound render contract and `CurrentOnlyUnbound` (`:211-247`). |
| Raw/detector ordering | Pass | Mandatory bounded three-pass protocol and fixed controls (`:249-298`). |
| Force/stale/config drift | Pass | Exact classes, preimages, force rules, integrated fixtures (`:314-378`, `404-426`). |
| Atomic mutation | Pass as design | Journal/rollback/failure injection contract (`:380-396`, `500-504`). |
| Legacy compatibility | Pass | Broad replacement and hidden fallback removed (`:199-204`, `369-372`). |

## Actual upstream authority block

The design correctly records the real deployment limitation: the checked-in
policy workflow grants only `contents: read` (`.github/workflows/ci-policy.yml:31-32`)
and uses the workflow token for API acquisition (`:64-70`). GitHub documents
that repository-ruleset `bypass_actors` is only returned to callers with write
access to the ruleset; repository ruleset required checks also use
`integration_id`, not `app_id` ([ruleset REST schema](https://docs.github.com/en/rest/repos/rules#get-a-repository-ruleset)).
Branch protection separately exposes required-check `app_id`
([branch-protection REST schema](https://docs.github.com/en/rest/branches/branch-protection)).

Therefore the protected path is currently **unavailable**, not inaccessible by
design failure. Until the upstream base-owned policy launcher supplies the
narrow `ruleset_bypass_read_authority` or equivalent trusted attestation, the
correct result is `baseline_policy_unproven` before rendering, with no output or
sidecar mutation (`:125-140`, `448-483`). Local explicit operator modes remain
usable. The generator must never grant PR code broader token access to repair
this.

This is the one separate prerequisite for G1 integration: wire and fixture the
trusted caller/attestation, including `/app`/installation identity, exact
ruleset bypass visibility, source-specific check IDs, and a positive protected
case. A read-only caller with omitted bypass fields must remain a negative case.

## Bounded implementation handoff

1. Implement local/operator mode plus protected acquisition that fails closed
   exactly as specified.
2. Add the integrated SI-B1–B4 and hostile fixtures on one frozen source SHA.
3. Complete the upstream trusted-authority task and positive protected fixture.
4. Independently review the immutable source candidate; only then assess G1.

No additional design blocker was found. Do not claim G1 approval from this
artifact or from local-only tests.

