# G0 estate-scope review (preliminary)

Review timestamp: 2026-09-19T18:44:49Z (UTC)

Status: independent design review only. No G0-G7 gate is approved. No source
files were changed. The g0-fleet worktree had no implementation diff or new
commit at review time; exact-commit review remains required.

## Scope and evidence inspected

- Source base: `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9` in `velnor3`.
- Fixed authority: `velnor-github-first-dual-lane-goal.md:21-58`. It says the
  list is exact, has 32 unique entries, and that live default branches are
  authoritative.
- Current fleet evidence snapshot: `G0/fleet/configs.tsv:1-33` (32 data
  rows). This is an external observation, not source authority.
- Existing estate config: `config/estate-repositories.json:1-19` and its
  `repositories` array. It has 28 entries. Comparing exact `name` strings to
  the fixed list gives 22 overlaps, 6 config-only names, and 10 fixed-scope
  names absent from config.
- Existing auditor: `crates/velnor-tools/src/audit_ci.rs:45,74-105,
  172-186,253-280,357-496,572-596`.

## Concrete false-green paths in base

1. `canonical_fleet_map` reads `config/estate-repositories.json` from the
   caller-selected `repo_path` and makes its 28 rows the canonical map
   (`audit_ci.rs:253-280`). A caller-selected checkout can therefore supply a
   different map, and the map is not the fixed 32-repository authority.
2. `--estate` parses a caller-supplied manifest, then validates it only against
   that 28-row map (`audit_ci.rs:363-395`). The failure text itself says
   “expected exactly 28 repositories” (`audit_ci.rs:584-593`). A 28-row estate
   matching the auxiliary file can pass scope while omitting 10 required
   repositories and carrying 6 unrelated repositories.
3. Estate work then iterates only the caller rows (`audit_ci.rs:387-468`), so
   no later aggregation can discover omitted fixed identities. A count check
   over 28 or a set check against the same caller/config source is circular.
4. A one-repository invocation with `--repository-name` only checks that the
   name exists in the 28-row map (`audit_ci.rs:480-495`). It cannot establish
   complete-fleet scope and must not be used as G0 evidence.
5. Class is inferred from name suffix/prefix (`audit_ci.rs:260-271`), so a
   substituted repository can receive a plausible class and evade a scope
   check unless identity is validated before class/audit work.

## Required authority boundary

The implementation must have one immutable, compile-included canonical
manifest (or an accepted-SPEC artifact with a pinned digest) containing the
exact 32 owner/repository strings. Scope validation must compare the observed
identity set to that manifest before cloning or auditing. It must fail closed
on all of the following: duplicate, missing, extra, substitution, case/space
or trailing-slash alias, owner/repository swap, malformed identity, and digest
mismatch. Do not normalize an alias into acceptance.

`config/estate-repositories.json` is auxiliary metadata only: concern
contracts, paths, and class/workload hints. Its 28 entries must never define
the expected count or set. The boundary must be explicit in types and output:

- auxiliary rows may be absent for fixed-scope identities, but that absence
  cannot shrink the authoritative set; rules that require auxiliary metadata
  must emit a blocking missing-auxiliary finding rather than silently pass;
- auxiliary rows outside the fixed set may be reported as out-of-scope
  metadata, but must not be promoted into the canonical set;
- duplicate auxiliary rows/keys must fail strict parsing; duplicate canonical
  identities must fail before map insertion;
- caller `--estate`, local directory names, `configs.tsv`, and result-row
  counts are observations/inputs, never authority;
- every authoritative identity must remain bound to a live remote identity and
  live default branch/head. A caller-provided branch or SHA cannot substitute
  for that lookup.

The output should separately identify `canonical_scope` and
`auxiliary_metadata`; a single merged map would recreate the current circular
trust. No source-side “28 expected” literal may remain.

## Adversarial fixture contract

`fixtures.json` contains the fixed 32 identities, the observed 28-row
auxiliary set, and mutation cases. The scope checker must produce these
outcomes before any repository audit:

| Fixture | Mutation | Required result |
| --- | --- | --- |
| `exact32` | fixed list unchanged | accept scope; still require live identity/default checks |
| `duplicate-missing` | duplicate `tailrocks/velnor`, drop `tailrocks/termpane` | reject with duplicate and missing |
| `missing-one` | drop `jackin-project/jackin-role-action` | reject missing; count is 31 |
| `extra-substitution` | replace `tailrocks/termpane` with `tailrocks/not-in-scope` | reject missing + unexpected |
| `case-alias` | use `Tailrocks/velnor` | reject; no case folding |
| `trailing-slash-alias` | use `tailrocks/velnor/` | reject; no URL/path normalization |
| `owner-repo-swap` | use `velnor/tailrocks` | reject malformed/unexpected |
| `auxiliary-28-only` | feed the current 28 config names as observed scope | reject scope; never reinterpret as the authority |
| `auxiliary-extra` | add `tailrocks/not-in-scope` to auxiliary metadata | keep it auxiliary and report out-of-scope; do not add to expected scope |
| `auxiliary-duplicate` | duplicate `tailrocks/velnor` in auxiliary rows | reject strict auxiliary parse |
| `caller-substitution` | caller supplies any 28-row or alternate manifest | reject against immutable fixed32, regardless of caller count |
| `caller-concern-substitution` | caller keeps all 32 names but rewrites required concerns to `non-applicable` | reject/compare against the accepted concern plan; caller metadata must not weaken the audit |
| `canonical-digest-mismatch` | alter one fixed identity while retaining count 32 | reject authority/digest mismatch |

The exact fixture file is intentionally operation-based so it can be reused by
Rust tests without embedding a second competing manifest. A test harness must
materialize each mutation, run the real parser/validator, and assert both the
exit result and diagnostic category.

## Review disposition

Preliminary constraints are relayed to the coordinator. I cannot approve the
g0-fleet change until its exact commit demonstrates an immutable fixed32
authority, a typed auxiliary boundary, exact identity-set equality, and the
fixtures above (including the 28-row false-green regression). The eventual
review must inspect source and tests at that commit; no design-only claim is a
gate result.

## Uncommitted g0-fleet design review (2026-09-19T18:50Z)

The isolated `/private/tmp/g0-estate-scope` worktree currently has an
uncommitted diff in `config/estate-repositories.json` and
`crates/velnor-tools/src/audit_ci.rs` (no commit SHA yet). The source config was
expanded from 28 to the exact 32 names, and the auditor added a compile-time
32-name array plus scope metadata. The names now compare equal to the fixed
goal list, but the following blockers remain:

- `canonical_fleet_map` calls `validate_fixed_repository_scope` on the file
  labelled `auxiliary-concern-projection` (`audit_ci.rs:305-318`). This turns
  auxiliary metadata cardinality/set into a hard prerequisite and collapses the
  authority boundary. Keep fixed scope validation independent; parse optional
  concern projections without requiring every fixed identity or rejecting
  out-of-scope metadata merely because it is auxiliary.
- `validate_estate_scope_metadata` checks only caller-controlled text fields
  (`audit_ci.rs:344-370`). The compile-time list is the real authority, but it
  has no pinned accepted-SPEC digest/content binding. A changed goal/list can
  remain count 32 and pass. Add a digest or immutable accepted manifest and a
  test that detects drift.
- The new required `scope` field broke an existing test fixture:
  `cargo test -p velnor-tools --bin velnor-tools` produced **206 passed, 1
  failed**; `audit_ci.rs:4155-4158` panics on `missing field scope`. This is a
  concrete unfinished-test failure.
- The diff has no executable adversarial tests for duplicate, missing, extra,
  case/trailing/owner aliases, caller 28-row substitution, or the auxiliary
  boundary. The external `fixtures.json` is a contract, not proof until tests
  materialize each mutation and assert rejection categories.
- The caller's exact32 rows still supply workload defaults and concern
  contracts (`audit_ci.rs:550`); `audit_concern_contract` treats
  caller-rewritten `non-applicable` classifications as informational
  (`audit_ci.rs:915-944`). A source/accepted concern plan must be authoritative
  or the caller projection must be compared before audit; otherwise an exact32
  manifest can weaken the audit without changing scope.

Therefore the design is not approved; exact-commit source/test review remains
pending.

## Exact commit review: `6387532f03c230f26795975ba5ff3034f2862eb7`

Review result (2026-09-19T18:56Z): bounded fixed-scope behavior passes; this is
not a G0-G7 gate approval.

- The compiled `GOAL_FLEET_REPOSITORIES` array (`audit_ci.rs:53-86`) has 32
  entries. The committed config has 32 entries. Exact sorted comparison against
  goal section 2 is empty (`config=32`, `goal=32`, no diff); no spelling or
  expansion typo found.
- `validate_fixed_repository_scope` (`audit_ci.rs:373-393`) rejects duplicate
  names, missing names, and unexpected substitutions. The four added hostile
  tests all pass: `fixed_scope_rejects_wrong_repository_substitution`,
  `fixed_scope_rejects_missing_repository`,
  `fixed_scope_rejects_duplicate_repository`, and
  `fixed_scope_rejects_extra_repository`.
- Full binary suite: **211 passed**. `cargo fmt --all -- --check` and
  `cargo clippy -p velnor-tools --bin velnor-tools --tests -- -D warnings` pass.
  Local and origin branch both resolve to `6387532f...`; worktree clean.
- Live default reconciliation remains fail-closed: `remote_default_identity`
  resolves each fixed identity via `git ls-remote --symref ... HEAD`
  (`audit_ci.rs:704-732`), then local checkouts must match the live branch and
  SHA (`audit_ci.rs:831-880`). The static fixed list does not replace this.

Residual authority-boundary finding remains. `audit_ci` validates names against
the fixed list, but then consumes caller-supplied estate defaults and concern
classifications for workload selection and contract auditing
(`audit_ci.rs:547-563`). `audit_concern_contract` treats a caller-rewritten
`non-applicable` classification as informational (`audit_ci.rs:915-944`), so an
exact32 caller manifest can weaken the accepted concern plan without changing
scope. Either bind the concern plan to the source/accepted manifest or record
this as an explicit out-of-scope boundary before G0 claims. The textual
`scope.authority`/section/count metadata is also not a content digest; the
compiled list is the effective authority, but no drift test binds it to the
goal artifact. Alias forms are rejected by exact set equality, though only the
substitution/duplicate/missing/extra variants have executable tests.
