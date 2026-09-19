# G1 scan-integrity amended-design review

Decision: **CONDITIONAL REJECT for implementation readiness.** The amended
design fixes the earlier SD-2/SD-3/SD-4 omissions and materially specifies SD-1,
but four trust-boundary/feasibility gaps remain. Source implementation and G1
approval stay paused.

## Exact input and scope

- Candidate commit: `458489729431822feed9c06c6678cb814a9be73f`, branch
  `codex/g0-estate-scope`, clean detached worktree `/private/tmp/g0-estate-scope`.
- Candidate tree: `3ed34f6afeecd4453b209bbdf42232aca3456e1e`.
- The commit is a pointer-only documentation commit (no tree diff); its commit
  message pins the external artifact below. No source, generated-file, host, or
  Docker operation occurred.
- Exact external artifact:
  `G1/scan-integrity/structural-design.md`, 345 lines / 20,456 bytes,
  SHA-256 `8d035aa1a583fe6828d3b0820e3800fee45f4e65fbbf670e964bba22b169de39`.
- Previous constraints and rejection were preserved and compared:
  `G0/fleet/scan-ownership-design-review.md` SHA
  `25940dcce1e2d7dc9680c2f3417cea937de0b733286523f66c3b57e27ad662ca`, and
  `G1/scan-integrity/review2.md` SHA
  `f1d32be2d95063476a5402bc95abca2f3c6cc1c93e8a655d5b5ffae1d1d7a1fb`.

## Capability matrix

| Requirement | Result | Evidence / remaining condition |
| --- | --- | --- |
| Sidecar/header/path/digest cannot authorize ownership | Pass | Explicitly claims-only (`structural-design.md:35-38`, `127-130`). |
| Protected API acquisition | Partial | Exact ref/commit/tree/blob/protection/ruleset calls, response retention, SHA check, post-read ref check, and fail-closed states are named (`:64-93`). Producer identity proof remains an imperative, not a protocol (`:74-80`). See R1. |
| Local operator baseline | Pass, subject to R2 | Full 40-hex `--baseline-revision`, object-db reads, no config/sidecar selection, diagnostics, no-write unavailable behavior, and no protected-mode weakening are explicit (`:95-125`, `240-244`). |
| No-baseline/source-unavailable behavior | Partial | Local `none` lifecycle is usable (`:110-118`, `289-315`), but a protected API failure is allowed to enter the same `none` mode by the ordering (`:187-190`). See R3. |
| Raw inventory versus detector inputs | Pass in principle | Named views and immutable prior/stale removal before render are explicit (`:146-169`, `196-205`, `316-320`). The second pass is called “optional” and its fixed-point bound/failure semantics are unspecified. See R4. |
| Ownership classes and force | Pass in principle | Current-prior, stale-exact, modified, and unknown classes plus preimage/force rules are explicit (`:132-144`, `224-244`, `263-284`). Current-only path authority remains ambiguous. See R2. |
| Config drift ordering | Pass | Drift is required before obsolete-renderer errors and no mutation is required (`:191-195`, `257-262`, `324-325`). |
| Stale removal | Pass in principle | Exact prior preimage + explicit force, edited stale refusal, and integrated fixture are specified (`:269-274`). Requires the baseline/source path in R1/R2. |
| Forged bytes/digest | Pass for baseline-bound paths | Sidecar-forged workflow/action fixture and force refusal are explicit (`:278-284`). Add the missing current-renderer/new-path adversary in R2. |
| Legacy behavior | Pass | Broad unowned replacement is explicitly removed; no alias/fallback (`:120-125`, `236-238`). |
| Mutation race/symlink safety | Pass in principle | Preimage/identity/ancestor checks and hostile fixtures are named (`:171-179`, `214-218`, `292-297`). “Apply atomically” needs an implementation transaction contract. |

## Blocking residuals

### R1 — protected producer identity is still not concrete

The API sequence is concrete, but `“Prove that the fixed base policy/D19
validator identity is the required protected producer”` (`:74-80`) does not say
what response fields constitute proof. A branch name, check title, or
repository-owned config cannot self-authorize this boundary. Before source work,
define the trusted caller contract and immutable acceptance set: repository/base
identity, validator revision/closure, GitHub App/installation identity, required
status-check context plus app identity, active ruleset/branch-protection
enforcement, and any bypass actors. Bind those values to the protected base
object set and reject missing/ambiguous identity. Retain the raw response and
viewer identity, but do not treat retention as proof.

Required future task: produce the upstream policy-caller artifact/API fixture
that supplies and verifies these values, including an API response where the
required check title exists but is produced by the wrong app. A candidate
worktree/config must not be able to choose the caller or accepted identity.

### R2 — current-renderer path can still be read as ownership authority

`GeneratedCurrentExact` requires only “current typed renderer path and bytes
equal current desired bytes” (`:138-144`). The design later says the renderer
“does not authenticate existing bytes” (`:200-202`), but it never states that
the current renderer/path contract itself is bound to the trusted baseline or
an authenticated generator closure. As written, a changed renderer/config that
emits a new `.github/workflows/evil.yml` or action, plus matching current bytes,
can reach the current-exact exclusion pass; a forged sidecar/header is not
needed. This is the same self-authority class under a different input.

Required future task: make current-exact proof mode-specific and explicit. In
protected mode, bind both path and desired bytes to the authenticated validator
and generator closure; otherwise classify a current-only/new path as
`ForeignOrUnknown` and keep it in `detector_inputs`/conflict. In explicit local
operator mode, document that the operator-selected binary is the authority.
Add an integrated hostile fixture that changes the renderer/config to emit a
new workflow/action whose bytes are exact, then asserts visibility, conflict,
no write, and no force bypass. SI-N1/SI-N2 as currently written do not cover
this case.

### R3 — protected baseline failure must not downgrade to local `none`

Step 1 says an unavailable protected baseline enters “explicit no-baseline”
mode (`:187-190`), while `baseline_source=none` may create missing current
outputs (`:110-118`). That is usable for an explicitly local first generation,
but unsafe/ambiguous for a protected policy caller: a 401/403/404, policy
identity failure, transport failure, or missing App could become a write-capable
local mode. `SI-B1` only says no fallback baseline is selected (`:301-308`); it
does not assert no output/sidecar writes on those failures.

Required future task: make source selection caller-bound. `none` is permitted
only for an explicit local operator invocation. Protected-base API failure is a
terminal `baseline_unavailable` with no output or sidecar mutation, and cannot
be converted by `--force`, environment, config, or sidecar. Extend SI-B1 to
assert this for every listed API failure.

### R4 — detector fixed point and fixed controls need a mandatory protocol

The two-view direction is correct, but the second current-exact pass is called
“optional” (`:159-164`) and “stable class set” has no bound, cycle behavior, or
failure result. “Fixed generator control files” are also not enumerated or
bound to immutable code (`:167-169`). An implementation could omit the second
pass or make a changed/cyclic renderer appear stable.

Required future task: specify a mandatory finite pass count/identity algorithm,
the exact no-fixed-point error and no-write behavior, and the immutable
code-owned control-path set. Config/sidecar/current renderer must never extend
that set. SI-B3 must assert the bounded failure path as well as the stable
positive case.

## Feasibility residuals to resolve before source review

1. The actual supported config can name an external generator repository
   (`.github-gen/velnor-workflow.toml` in the candidate says
   `repository = "tailrocks/velnor"`). The protected contract lists target
   `/repos/{owner}/{repo}` object calls, but does not define acquisition and
   verification of a generator commit/tree/source closure in a separate repo.
   Add that object/release source or fail `baseline_generator_unproven`; never
   treat the target config's slug or a local HEAD as proof.
2. Existing policy semantics permit a missing `[generator].revision` and use
   the protected entrypoint pin as a fallback. The design says to read a full
   revision from a base config blob (`:81-84`) but does not define this fallback
   or an explicit fail-closed result. Choose one and fixture it.
3. `operator-commit` calls the selected commit immutable (`:97-106`) but does
   not state Git object verification/no-replace-object handling. The
   implementation must read raw objects by full IDs, verify type/tree/blob
   identities, and reject replace/graft/alternate tricks if this mode is used
   for a security-sensitive check.
4. “Apply atomically” (`:214-218`) needs a bounded per-file/multi-file
   transaction/rollback definition and an injected write-failure test proving
   no partial output or sidecar projection. This is implementation acceptance,
   not permission to reintroduce the old broad force path.

## Verdict and next bounded work

The amendment genuinely fixes the prior local-CLI lifecycle, raw-versus-
detector ordering, class/force wording, stale/config-drift fixtures, and
source-unavailable taxonomy. It is a feasible direction, but not yet safe to
implement because R1–R3 leave source selection/renderer authority capable of
self-authorization; R4 leaves the detector protocol underspecified.

Next work, in order:

1. Specify and independently fixture the protected policy/App identity and
   external generator object acquisition.
2. Resolve current-only path classification and protected-API failure versus
   explicit-local-`none` behavior; add the two hostile fixtures.
3. Make detector fixed-point/control exclusions mandatory and bounded.
4. Re-review one new immutable design artifact; only then review a fresh source
   candidate. Do not run implementation tests against a moving worktree or
   claim G1 approval from this document.

