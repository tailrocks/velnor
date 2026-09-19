# G0 estate-scope exact review — `58bd26f905624820042ff944997ea21476fc0127`

Review scope: clean detached worktree `/tmp/velnor-estate-review-58bd26f`,
branch `codex/g0-estate-scope`, exact commit `fix(audit): reject unknown
estate fields`. No source, owner worktree, remote, or user-authored evidence
files were changed.

Inputs reread at review time:

- goal SHA-256: `c9bdb89b302b3fc1ebfb8ff43123590f425f03dbd8855f62bf3a2b42b648b647`;
- external report SHA-256: `001f4ad4469ff82d8d27ba720654f72e0972d32245fe62a560348dada3941590`;
- external fixtures SHA-256: `19513137fa6b08db2b24513fa36dc361e74a21ddf530e670b348320c41d662de`.

## Verdict

**Do not approve G0.** The bounded fixed-scope and caller-plan safety paths
are materially improved and the exact source test suite is green. The exact
commit still lacks an immutable digest/drift proof for the 32-name authority,
rejects an auxiliary-extra fixture that must remain auxiliary, and has no
executable regression suite for the full adversarial fixture contract.

## What passes

- Goal section 2, compiled `GOAL_FLEET_REPOSITORIES`, and committed config each
  contain 32 entries; sorted goal/compiled/config identity comparison is empty.
  The compiled list drives `fixed_goal_classes` and
  `validate_fixed_repository_scope`; the auxiliary config no longer defines
  the expected fleet set.
- Four fixed-scope unit tests reject substitution, missing, duplicate, and
  extra identities. Exact CLI preflight also rejected duplicate/missing,
  extra substitution, case alias, trailing-slash alias, owner/repository swap,
  and the external 28-row auxiliary-only/caller-substitution fixture. Exact
  comparison does not normalize aliases.
- `#[serde(deny_unknown_fields)]` rejects hostile top-level, scope,
  repository, and concern fields in the real CLI. The new unknown-field unit
  test passes.
- Caller concern weakening is now source-bound. `validate_auxiliary_contract_binding`
  rejects an exact32 caller manifest rewriting all defaults and repository
  concerns to `non-applicable`; the estate loop then uses the accepted
  auxiliary repository/default plan (`audit_ci.rs:592,597-605,653-675`), not
  caller values. This closes the residual 638 caller-plan path; it is not a
  remaining false-green finding.
- Live identity remains authoritative in the implementation: each estate row
  calls `remote_default_identity` before checkout/audit, and local checkouts
  must match the resolved branch, SHA, and clean state. A read-only
  `git ls-remote --symref` sample resolved `tailrocks/velnor` to `main` and a
  40-hex head.

## Remaining blockers

### 1. No pinned digest binds the compiled 32 names to the goal artifact

`GOAL_FLEET_REPOSITORIES` is a compile-time array, but there is no accepted
goal/manifest digest or test that detects drift against the goal file. The only
digest (`ACCEPTED_AUXILIARY_CONTRACT_SHA256`) covers serialized auxiliary
defaults and rows, not the canonical 32 identities. `validate_estate_scope_metadata`
checks caller text plus count and that auxiliary digest. A changed compiled
list can therefore become the effective authority without a goal-content
drift failure. The external `canonical-digest-mismatch` fixture currently
produces only missing/extra (identity substitution) or an auxiliary
`scope contract digest` error; it cannot produce the required
`authority-digest-mismatch` because no canonical identity digest exists.

Add one accepted immutable 32-identity artifact/digest and a test comparing
the compiled/list source to it. Keep that digest independent from the
auxiliary concern projection.

### 2. Auxiliary-extra is incorrectly a hard parse failure

`validate_auxiliary_repository_scope` (`audit_ci.rs:406-422`) rejects any
auxiliary row outside the fixed set. The exact CLI mutation of the current
config produced:

`auxiliary estate manifest contains out-of-scope repositories: ["tailrocks/not-in-scope"]`

The fixture contract requires auxiliary-extra to parse/report as out-of-scope
metadata while never adding it to canonical scope. Preserve the fixed 32 set,
but report auxiliary extras separately instead of rejecting them as scope
authority.

### 3. Missing auxiliary metadata is blocked by whole-projection digest

Removing `tailrocks/termpane` from the canonical auxiliary rows leaves the
compiled fixed scope at 32, but fails before audit with
`canonical estate manifest auxiliary contract digest mismatch`. This does not
shrink scope, which is safe, but it couples auxiliary cardinality/content to a
hard startup gate and emits no typed missing-auxiliary finding. The required
boundary permits absent auxiliary rows; metadata-dependent checks should block
with an explicit missing-auxiliary result while fixed scope remains complete.
Either make optional-row semantics explicit and preserve the pinned accepted
plan for available rows, or document/encode why the entire projection is an
intentional mandatory contract and update the fixture contract accordingly.

### 4. External adversarial fixture contract is not executable coverage

`cargo test -p velnor-tools --bin velnor-tools` passes 214 tests, including
the four set mutations, direct auxiliary digest/concern tests, and unknown
field parsing. It does not materialize and assert every `fixtures.json` case:
case/trailing/owner diagnostic categories, auxiliary-extra acceptance/report,
auxiliary-duplicate boundary, auxiliary-28-only, caller concern rewrite with
default inheritance, and canonical authority digest drift are absent as
regressions. The manual CLI checks above prove current rejection behavior, but
must become source tests before G0 evidence.

## Exact verification

- `rtk cargo test -p velnor-tools --bin velnor-tools` — **214 passed**.
- Focused fixed-scope tests — **4 passed**; hostile concern binding and
  auxiliary digest drift — **1 passed each**.
- `rtk cargo fmt --all -- --check` — passed.
- `rtk cargo clippy -p velnor-tools --bin velnor-tools --tests -- -D warnings` —
  passed.
- Exact detached tree was clean at review completion.
- `audit-ci --repo-path . --estate config/estate-repositories.json --offline`
  accepted the unmodified exact32 scope and stopped at the intentional
  freshness guard (`estate audit cannot skip delivered-default freshness
  checks`); no repository audit was falsely counted as G0 evidence.

This is an independent exact-commit review, not a G0-G7 approval. The next
source revision needs the canonical identity digest/drift test, the auxiliary
boundary correction, and executable materialized fixture coverage.
