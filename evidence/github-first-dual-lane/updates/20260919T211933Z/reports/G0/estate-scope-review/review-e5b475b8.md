# G0 estate-scope exact review — `e5b475b80110447469fdeb4a24b7967008b39aca`

Review scope: clean detached tree `/tmp/velnor-estate-review-e5b475b8`, exact
`codex/g0-estate-scope` commit `test(audit): cover accepted concern
execution`. No source, owner worktree, remote, or prior user-authored
evidence files were changed.

Inputs reread at review time:

- goal SHA-256: `c9bdb89b302b3fc1ebfb8ff43123590f425f03dbd8855f62bf3a2b42b648b647`;
- external report SHA-256: `001f4ad4469ff82d8d27ba720654f72e0972d32245fe62a560348dada3941590`;
- external fixtures SHA-256: `19513137fa6b08db2b24513fa36dc361e74a21ddf530e670b348320c41d662de`.

## Verdict

**Exact bounded implementation review: pass, subject to the duplicate JSON
key caveat below. No G0-G7 gate approval.** This commit closes the four prior
authority-boundary findings and exercises the user fixture contract in source
tests. The raw JSON parser still accepts duplicate object keys; if “duplicate
keys” means JSON map keys (not only duplicate repository rows), that remains a
strict-parser blocker.

## Authority and boundary proof

- Goal section 2, compiled `GOAL_FLEET_REPOSITORIES`, and committed config each
  contain 32 identities; exact sorted comparisons are empty.
- The goal identity digest is independent and exact:
  `ef6fa934b7b0431579aad8c92e2c5386b48e49210055249113659a03099b1f79`.
  The source computes the newline-joined goal list digest and validates it
  before scope acceptance. Shell recomputation from the goal file produced the
  same digest.
- The auxiliary concern projection has a separate digest:
  `b9b7b43d2ab562bec802f7aca3f85c2e7f7aba1f5da92d762e083e59271c94b8`.
  Auxiliary rows are filtered to fixed identities with nonempty concerns for
  this digest; extra metadata cannot alter canonical scope.
- JSON estate output now separates `canonical_scope` from
  `auxiliary_metadata`; auxiliary output reports `present`, `missing`,
  `out_of_scope`, and diagnostics.
- `validate_estate_scope` labels caller input as non-authoritative, validates
  exact set equality and identity diagnostic tags, then validates the
  independent goal digest.
- The accepted concern plan is used at runtime. Caller rewrites are rejected
  by `caller-plan-not-authority`/`concern-plan-mismatch`, and the execution
  test proves required `rust-ci` remains required rather than becoming
  informational `non-applicable`.
- Missing auxiliary metadata cannot shrink the fixed set: the missing-row test
  reports `tailrocks/termpane` while `fixed_goal_classes()` remains length 32.
  A missing row with nonempty concern data fails the pinned auxiliary digest
  closed; it does not reduce scope.
- Auxiliary extras are accepted as metadata and reported as
  `auxiliary-out-of-scope`; they are not promoted. Duplicate auxiliary rows
  reject with `duplicate-auxiliary`.
- Live identity remains authoritative: each estate row calls
  `git ls-remote --symref https://github.com/{owner}/{repo}.git HEAD`, then
  local/remote checkout validation binds branch, SHA, and clean state. A
  read-only sample resolved `tailrocks/velnor` to `main` and a 40-hex head.

## User fixture results

The 16 fixture unit tests passed. Real CLI preflight was also exercised with
the exact config and `--offline`; valid scope reaches the intentional freshness
guard, while hostile inputs fail before repository audit:

| Fixture | Exact result |
| --- | --- |
| exact32 | scope accepted; stopped at delivered-default freshness guard |
| duplicate-missing | rejected; `duplicate=true` plus missing `termpane` |
| missing-one | rejected; missing role-action |
| extra-substitution | rejected; missing + `unexpected` extra |
| case-alias | rejected; `alias-not-normalized` |
| trailing-slash-alias | rejected; `alias-not-normalized` |
| owner-repo-swap | rejected; `malformed-identity` |
| auxiliary-28-only / caller-substitution | rejected against fixed32 with caller-input-not-authority, missing, unexpected |
| auxiliary-extra | canonical auxiliary parse accepted; CLI reached freshness guard; unit output reported out-of-scope |
| auxiliary-duplicate | rejected with `duplicate-auxiliary` |
| caller-concern-substitution | rejected with caller-plan/concern-plan mismatch |
| canonical-digest-mismatch | source test rejected changed list with authority-digest-mismatch |

Unknown top-level, scope, repository, and concern fields reject through the
real CLI (`deny_unknown_fields`).

## Strict-parser caveat

`serde_json` map deserialization does not reject duplicate object keys. Feeding
the real CLI a manifest with the same `scope.role` key twice, or the same
`defaults.lane-selection` key twice with identical values, was silently
accepted and reached the freshness guard. Existing tests cover duplicate
repository rows, not duplicate JSON keys. If strict duplicate-key rejection is
part of the accepted auxiliary contract, add a duplicate-detecting parser or
pre-parse and a regression fixture. This caveat does not permit scope
substitution because exact set/digest checks still run.

## Exact verification

- `rtk cargo test -p velnor-tools --bin velnor-tools` — **227 passed**.
- Fixture subset — **16 passed**; missing projection, goal digest, and hostile
  concern execution targeted tests passed.
- `rtk cargo fmt --all -- --check` — passed.
- `rtk cargo clippy --workspace --all-targets --all-features -- -D warnings` —
  exit 0. The build emitted one known `velnor-runner` release-build advisory
  warning; no clippy error.
- Exact detached tree clean at completion.

This confirms the corrected source boundary and fixture behavior only. It is
not live 32-repository G0 evidence or a G0-G7 completion claim.
