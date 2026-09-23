# Velnor consolidation ledger

Status: **OPEN/UNPROVEN**

Updated: 2026-09-23

Repository: `tailrocks/velnor`

Baseline `main` at initial ledger creation: `6391a0ce53b7e666d1bfb391083011622158f4f3`

Refresh `main` before this PR: `0eeac75a157da9c1e4171d56b663fe5943e85fed`

Final `main`: **OPEN/UNPROVEN** until cleanup, fresh discovery, and clean
checkout verification are complete.

The ledger is project-facing. It omits personal paths, usernames, credentials,
tokens, raw stash contents, ignored-file contents, and private recovery
locations. Machine-specific recovery identifiers refer to the private recovery
record, not to a public filesystem path.

## 1. Evidence and decision vocabulary

This ledger follows the evidence rules in
[`content/docs/guides/contributing.mdx`](../content/docs/guides/contributing.mdx),
the current commands and proof boundaries in
[`content/docs/guides/development.mdx`](../content/docs/guides/development.mdx),
and the behavior-to-capability method in
[`plans/2026-09-17-pr994-behavior-ledger.md`](2026-09-17-pr994-behavior-ledger.md).
The older behavior ledger is historical evidence, not an instruction to copy
another project's generated files or revive rejected architecture.

Claims use these labels:

- **CURRENT** — verified against the named ref or live repository state.
- **HISTORICAL** — preserved point-in-time evidence; requires refresh before a
  destructive decision.
- **FUTURE** — intended follow-up, not implemented by this ledger.
- **OPEN/UNPROVEN** — source or claim remains under review, has a failing gate,
  or lacks final integrated proof.

Disposition values:

- **ACCEPT AS-IS** — the contribution is landed and its logical payload is
  accepted on `main`.
- **ACCEPT WITH ADAPTATION/REIMPLEMENTATION** — only the stated subset may be
  reused after fitting current architecture and tests.
- **ALREADY SATISFIED** — current `main` already proves the behavior.
- **SUPERSEDED/REJECTED** — the source is not an integration target; the reason
  is recorded.
- **BLOCKED** — the useful change is preserved, but a demonstrated dependency
  or gate prevents acceptance.

## 2. Authority, governance, and access

### 2.1 Repository authority — CURRENT

- Verified repository identity: `tailrocks/velnor` at
  `https://github.com/tailrocks/velnor`.
- Only configured relevant remote observed: `origin`, with credentials omitted
  from this document.
- `refs/heads/main` was re-read directly and matched
  `0eeac75a157da9c1e4171d56b663fe5943e85fed` after a normal fast-forward
  refresh of this ledger branch from the prior baseline.
- Ruleset `protect-main` (`19573071`) requires pull requests, linear history,
  squash merge, DCO, Policy, `ci-required`, and review-thread resolution; it
  does not require a numeric approval count in the observed configuration.
  Ruleset `protect-tags` (`19573007`) protects tag deletion and non-fast-forward
  updates. No bypass was used.
- GitHub-generated pull-request refs are not contributor branches and are not
  deletion targets.
- `actions/runner` remains the protocol source of truth for runner-protocol
  claims. This ledger does not assert protocol behavior from guesswork.

### 2.2 Access limits — CURRENT/HISTORICAL

- Organization-level ruleset details were unavailable to the repository
  integration (GitHub returned HTTP 403 for the organization endpoint). The
  repository rulesets above are the accessible governance evidence; this is an
  access limit, not proof that no organization rule exists.
- Historical PR #826 had a deleted or inaccessible head repository. Its
  unavailable source cannot be treated as reviewed or safe to delete.
- The host inventory covered accessible development, hidden, temporary, and
  mounted roots with metadata-first Git identity checks. One duplicate system
  volume was excluded to avoid double-counting; no targeted permission error
  was recorded. The coverage claim does not include inaccessible or deleted
  remote history.

## 3. Discovery coverage

### 3.1 Host inventory — HISTORICAL snapshot

Inventory snapshot: `HOST-SCAN-20260922-225213Z`.

The scan checked ordinary and linked worktrees, independent clones, bare and
separate Git directories, detached heads, local branches, upstream state,
staged and unstaged changes, untracked and ignored files, stashes, operation
state, reflogs, recovery refs, unreachable-object candidates, submodules,
sparse/index state, alternates, and active writers.

Stable opaque inventory IDs and observed state:

| ID | Repository family | Observed coverage | State at snapshot |
| --- | --- | --- | --- |
| `HOST-VELNOR-PRIMARY-38c31bf79263f8c0` | Retained primary common store | 57 registered worktree entries; 18 stash entries | Dirty/conflicted; active writers; `CHERRY_PICK_HEAD`, auto-merge state, merge-resolution state, staged changes, conflicts, untracked files, and ignored files present |
| `HOST-VELNOR-CLONE-B-69b3f2a5817f4cf7` | Independent Velnor clone | 22 registered entries; 12 present and 10 missing | Merge-resolution metadata present; no deletion approved |
| `HOST-VELNOR-CLONE-C-7351cea9cb48abb0` | Independent Velnor clone | 370 registered entries; 37 present and 333 missing | Dirty state and merge-resolution metadata present; missing entries require investigation |
| `HOST-VELNOR-RELATED-1f3f828b8da1787e` | Related Velnor checkout/work clone | Git identity and nested-repository checks | Retained pending dependency and dirty-state review |
| `HOST-RELATED-REPOSITORIES` | Bastion, Homebrew, apt, and actions-fixture repositories | Remote/history identity checked | Unrelated repositories retained |

The primary checkout was deliberately not reset, cleaned, committed, or
deleted. Identical heads were not deduplicated over different indexes, dirty
files, stashes, or untracked material.

### 3.2 Remote and pull-request inventory — HISTORICAL snapshot

Remote inventory snapshot: `REMOTE-SCAN-20260922-224554Z`, digest
`0a56c4aceff852dcb8c0ecf7d90352a2b8de60db4364ed461a15999f9f6e07d6`.

The complete paginated remote scan observed 2,356 advertised refs, 44 branch
refs, 1,086 pull-request head refs, 8 pull-request merge refs, 549 archive
refs, 68 backup refs, 390 direct tags, and 326 releases. No accessible Velnor
forks were found. The counts are a historical coverage record, not a current
claim after subsequent pushes.

The scan also read relevant open, merged, and closed pull requests, including
descriptions, commits, reviews, issue comments, replies, and inline review
threads where accessible. Synthetic merge refs were not treated as source
branches.

### 3.3 Recovery and dirty-state coverage — OPEN/UNPROVEN

- Recovery record: `RECOVERY-VELNOR-20260922`.
- Historical bundle and object checks were internally verified, including
  round-tripping 3,124 refs, preserving 18 stash entries, and checking 65,661
  objects in the earlier coherent capture.
- The live inventory changed after that capture. No stable, source-coherent
  final capture of the current dirty primary checkout has been established.
- Dirty indexes, worktrees, stashes, ignored files, operation metadata, and
  local-only payloads therefore remain retained. A recovery record is not a
  disposition for any source and is not evidence that cleanup is safe.

## 4. Material change dispositions

The source SHA is the immutable reviewed PR head. For merged PRs, the
destination is the squash commit on `main`; for open or rejected PRs, no
destination is claimed. A GitHub `merge_commit_sha` shown for an open PR is a
synthetic test merge candidate and is not listed as landed.

### 4.1 Accepted and landed — CURRENT

| ID | Source base..head | Contribution | Disposition and evidence | Destination on `main` |
| --- | --- | --- | --- | --- |
| `PR-1084` | `a9fa5618381de1ce288f82bdc4cbdeda821f9130..13a0e2c3b0b8dad6c23c07c34791ec00443b4806` | Typed Apple validation phases for SwiftPM, XcodeGen, and shared Xcode schemes; phase selection and execution tests | **ACCEPT AS-IS.** PR-specific verification recorded 2,529 workflow-library tests, 59 Swift scanner tests, focused phase/execution tests, format, clippy, diff, and generator checks passing | `f7bc191899aa0c8013cf50789ae21b75f4c2468d` |
| `PR-1085` | `a9fa5618381de1ce288f82bdc4cbdeda821f9130..43ab3507044a2ad2a25283124aa2257f0affdb58` | Apple native identity transport and cache hardening | **ACCEPT AS-IS.** Merged through the protected workflow; current Apple identity/cache behavior is represented in the landed squash | `d4443aaee75c073b61a2f14daf02b3cafd104f39` |
| `PR-1090` | `d4443aaee75c073b61a2f14daf02b3cafd104f39..8c84b7b86ca964236573b15820f1273310c3ba54` | Restore blocking mode for accepted fake `Contents` streams on macOS | **ACCEPT AS-IS.** Corrects the structural listener-to-stream nonblocking inheritance issue; merged and retained | `61b6edd61d6e338bf8b7169f4d741f5b6fa06c2e` |
| `PR-1088` | `61b6edd61d6e338bf8b7169f4d741f5b6fa06c2e..c0bc5c97be601c8dfbf398f04519bbc21e488613` | Swift scan parity, cache/watch gaps, schema-1/S2 pin files, and comment/trivia-aware executable handling | **ACCEPT AS-IS.** Merged after focused scanner and generated-output verification | `e9b534ee1cf0ab6c833c2904be866054bad455ae` |
| `PR-1089` | `d4443aaee75c073b61a2f14daf02b3cafd104f39..0aafb5bce6a9422b4edade82e29d1fb5944673b8` | Linux-only tooling/cache compatibility, schema-1 mise closure, permit-ledger metadata seed support, raw-store precondition tests, and runner artifact-probe shutdown coverage | **ACCEPT AS-IS.** Merged through protected checks; source subsets were compared independently rather than treating the branch as a blind whole-branch merge | `45ba3841fe9fbbfa0cbdfcfd81d1667a9ab3e7b2` |
| `PR-1091` | `45ba3841fe9fbbfa0cbdfcfd81d1667a9ab3e7b8..1d203854ce264103de456a8236dc9c3bfb383430` | Permit-ledger schema readiness: required objects and singleton `permit_meta(id=1)`, with idempotent repair/test coverage | **ACCEPT AS-IS.** Merged after schema and idempotence verification | `52dc35b11aa190b62c082e70972b26c203598ef7` |
| `PR-1095` | `52dc35b11aa190b62c082e70972b26c203598ef7..e526ce80f80d51b8b96de5106434e2bf04d1170f` | Promote the published runtime product and generated runtime pin | **ACCEPT AS-IS.** The pin points at published runtime target `45ba3841fe9fbbfa0cbdfcfd81d1667a9ab3e7b2`; this is not a claim that later activation work is complete | `cdee4acda4c0f4bcc01c102a9e0a4bb6a173bacb` |
| `PR-1100` | `cdee4acda4c0f4bcc01c102a9e0a4bb6a173bacb..17df0a53a7a03ba36463dbeeb5a7eb2a957a65d4` | Raw-store quarantine before cleanup, descriptor-relative identity checks, and TOCTOU regression coverage | **ACCEPT AS-IS as landed, with a required follow-up.** Cross-UID pathname-race protection landed, but independent review found unbounded successful quarantine-container/manifest retention; PR #1106 addresses that liveness/resource defect and is not yet accepted | `507e722618371256cf1d2efe5e8dec2399d66650` |
| `PR-1103` | `507e722618371256cf1d2efe5e8dec2399d66650..fa437b465ea5cdb5ca87231311aa3972ea1608f0` | Normalize static source paths by rejecting direct, leading, or repeated `.github` components while preserving the original path | **ACCEPT AS-IS.** Two independent exact-head reviews passed; required PR CI run `35796867457` passed; generated ownership stayed unchanged | `6391a0ce53b7e666d1bfb391083011622158f4f3` |
| `PR-1093` | `6391a0ce53b7e666d1bfb391083011622158f4f3..f9cf4a16fceb01fc9d5cedc333032ea8fa5dcc6d` | Schema-1 mise install closure, lane-specific vectors, declaration-only units, canonical S2 aliases, and strict qualified-alias validation | **ACCEPT AS-IS.** Required PR CI run `35798591914` completed successfully; the PR then merged through the protected workflow. The landed mapping is current; cleanup and final-host reconciliation remain open | `0eeac75a157da9c1e4171d56b663fe5943e85fed` |

### 4.2 Rejected or superseded — CURRENT/HISTORICAL

| ID | Source base..head | Contribution | Disposition and reason | Destination |
| --- | --- | --- | --- | --- |
| `PR-1096` | `cdee4acda4c0f4bcc01c102a9e0a4bb6a173bacb..96d856c47a002f6568e971712e4e3f476c725f62` | Generated-tree D19 pin promotion | **SUPERSEDED/REJECTED.** Control-only pin referenced no published runtime product and could not prove generated-tree closure; closed not planned | None |
| `PR-1099` | `cdee4acda4c0f4bcc01c102a9e0a4bb6a173bacb..0a5e2534a8fc0d89c1dad6b7f7cbb6ec06dff489` | Attested renderer activation | **SUPERSEDED/REJECTED.** Only a policy-cancelled phase was complete; active workflows still used candidate manifests, polling, and legacy publication. Run/result identity, merge-group trust, hosted activation, final publication, rollback, and legacy retirement were unproven | None |
| `PR-1102` | `507e722618371256cf1d2efe5e8dec2399d66650..1e40998b6928d0cbbcbd8c3a99e35cdd88c32a31` | Continuation of renderer activation | **SUPERSEDED/REJECTED.** It regressed #1100's quarantine flow by restoring pathname-based cleanup after an FD check. Required CI run `35793300519` failed before substantive verification; candidate qualification alone was insufficient. Strict identity/result/reuse and protected publisher mechanics remain future adaptation material, not landed code | None; closed not planned |
| `PR-1094` | `52dc35b11aa190b62c082e70972b26c203598ef7..3f7633e118cdf32ec81df58a6acfc0e5a2c3248e` | Earlier action-scanner adaptation | **SUPERSEDED/REJECTED.** Its unpublished D19/runtime adaptation was superseded by the current action-contract sequence; no code was claimed merged | None |

### 4.3 Open or pending source — OPEN/UNPROVEN

| ID | Source base..head | Contribution | Current evidence and disposition | Destination |
| --- | --- | --- | --- | --- |
| `PR-1076` | `1e094e97c3ba86ec649a5c88589eaceeec485543..36f0d4f4981e125dacec96eaf82f24369d38251b` | Earlier validation-phase retention attempt | **SUPERSEDED/OPEN/UNPROVEN.** Older phase-retention source; later attempts exposed composition, workspace-scheme, schema-1, legacy-retention, and project-matching gaps. No landing | None |
| `PR-1087` | `a9fa5618381de1ce288f82bdc4cbdeda821f9130..9cf7f8470a7b73ba0f338ccd40c4a1d3450eb9e8` | Selective action scanner integration | **SUPERSEDED/OPEN/UNPROVEN.** Retained as historical source; current safe path requires a shared production action contract and caller migration | None |
| `PR-1092` | `45ba3841fe9fbbfa0cbdfcfd81d1667a9ab3e7b2..dce6cea7d26dab187ad01eadffb6c17966d8b674` | Phase retention continuation | **SUPERSEDED/OPEN/UNPROVEN.** Older open continuation; the current #1097 source is the active successor, but its own review findings remain unresolved | None |
| `PR-1097` | `cdee4acda4c0f4bcc01c102a9e0a4bb6a173bacb..19424b242ff06b363b1feeda1fa02858e5bb34a9` | Validation-phase retention capability | **OPEN/UNPROVEN.** Required CI run `35785105614` completed successfully, but independent reviews found non-composing distinct preconditions, XcodeGen workspace ownership mismatch, missing schema-1 compatibility gate, legacy S2 retention, substring project matching, and reject-only override behavior | None |
| `PR-1098` | `cdee4acda4c0f4bcc01c102a9e0a4bb6a173bacb..e56b46fc206b2001d49469103a70eba8f07d3779` | Action scanner adaptation on published runtime | **OPEN/UNPROVEN.** Required bootstrap closure was not proven and the shared action contract was not yet migrated; architecture review requires a contract-first sequence. No product or pin is accepted from this PR | None |
| `PR-1101` | `507e722618371256cf1d2efe5e8dec2399d66650..d4449f9db9919423853310dbcfa3dd4c23597810` | Debian package output isolation from stale cache state | **OPEN/UNPROVEN.** Required CI run `35795392879` completed successfully and focused tests passed, but independent exact-head reviews found broad target deletion and symlink-component escape gaps. Required adaptation: exact roots, no broad wildcard, and symlink-safe tests | None |
| `PR-1104` | `6391a0ce53b7e666d1bfb391083011622158f4f3..1ec6508b5d8da756ad6bcdc70b9c4743ab155854` | Standalone runner action-metadata contract foundation | **OPEN/UNPROVEN.** Required CI run `35798263048` failed. The crate has focused parsing and boundary evidence, but no production callers, scanner migration, generated output, runtime pin, or activation closure were included | None |
| `PR-1105` | `6391a0ce53b7e666d1bfb391083011622158f4f3..13b3860349f0b76f578c3c06b4761ecd86ea0d1c` | Runner process-group capture, timeout kill, pipe drain, and descendant-output tests | **OPEN/UNPROVEN.** Required CI run `35798543497` completed successfully; focused tests, format, diff, and clippy passed. Full nextest had 2,587 runs, 2,584 passes, 5 skips, and three timing/resource benchmark failures; exact-head review and integrated decision remain open | None |
| `PR-1106` | `6391a0ce53b7e666d1bfb391083011622158f4f3..68f8a7c6df32a377f6948d0b694d537eb54f9962` | Reclaim completed raw-store quarantine containers, preserve mismatches/malformed state, bound recovery state, and test restart/resource limits | **OPEN/UNPROVEN.** Local verification reported 379/379 full tests, 18/18 focused cleanup tests, format, clippy, and check passing; protected CI run `35798724524` failed. No merge or main behavior is claimed | None |

## 5. Local source-family dispositions

These IDs identify material local work that was not safely represented by a
single public PR at audit time. Exact paths and dirty contents remain private.

| ID | Immutable source evidence | Disposition | Public destination |
| --- | --- | --- | --- |
| `LOCAL-DEBIAN-42fb5e3c53a9c348842ba8791edb6dccfa25aa4c` | Debian reset candidate; evolved into PR #1101 head `d4449f9db9919423853310dbcfa3dd4c23597810` | **ACCEPT WITH ADAPTATION/REIMPLEMENTATION.** Keep the exact-root reset intent; reject broad globs and symlink-following paths | Pending #1101 |
| `LOCAL-SCHEMA1-567d4a13d0d44a0294c64bc818b56ab33e7d94c7` / `LOCAL-SCHEMA1-ed404ed320c13923c6942521bf79d9e818fc9555` | Local schema-1 candidates; evolved into PR #1093 head `f9cf4a16fceb01fc9d5cedc333032ea8fa5dcc6d` | **ACCEPT WITH ADAPTATION/REIMPLEMENTATION.** The validated subset landed through #1093; no remaining local copy is an independent accepted destination | Landed via #1093 |
| `LOCAL-ACTION-0670145165acedd276f2e4d4dc2cccee60326cde` | Dirty action-scanner continuation derived from the #1087 family | **ACCEPT WITH ADAPTATION/REIMPLEMENTATION.** Path/reference behavior is useful only after the shared production action contract; do not merge the mixed branch wholesale | Pending successor to #1104/#1098 |
| `LOCAL-PHASE-cf61d3e9a8a0c853b5cf0471bb7acc15d6cc3c42` | Local phase-retention implementation family | **ACCEPT WITH ADAPTATION/REIMPLEMENTATION.** Retain only after distinct-precondition composition, workspace ownership, schema-1 gating, legacy retention, and exact project matching are fixed and tested | Pending #1097 successor |
| `LOCAL-APPLE-CURRENT` | Audited Apple/Swift worktrees and dirty states; no single public immutable head for the mixed state | **ACCEPT WITH ADAPTATION/REIMPLEMENTATION.** SwiftFormat/SwiftLint detection, literal product facts, native ownership/deployment parsing, symlink boundaries, and universal macOS expansion are candidates; blanket mandatory identity is rejected | Future PR; no landed destination |
| `LOCAL-IDENTITY-CURRENT` | Audited identity-renderer integration state; raw state digest is private | **ACCEPT WITH ADAPTATION/REIMPLEMENTATION.** Renderer integration may proceed; plan-digest reuse is rejected until identity and trust semantics are proven | Future PR; no landed destination |
| `LOCAL-EXECUTOR-7352430e57808a5000fc816aae8c6f95c9157a11` | Process-execution source family; accepted subset is PR #1105 | **ACCEPT WITH ADAPTATION/REIMPLEMENTATION.** Process-tree capture and cleanup are in scope; invalid `{run}:` scanner/renderer changes are rejected | Pending #1105 decision |
| `LOCAL-ACTION-CONTRACT-CURRENT` | Contract foundation is preserved publicly as PR #1104 | **OPEN/UNPROVEN.** Foundation requires a passing protected gate and caller migration before action-scanner behavior can be accepted | Pending #1104 and successor |

No dirty local state, stash, detached commit, or recovery archive is itself an
accepted destination. Every such item remains subject to the same logical
change disposition.

## 6. Landing and verification record

### 6.1 Landed mapping — CURRENT

| PR | Squash destination | Verification record |
| --- | --- | --- |
| #1084 | `f7bc191899aa0c8013cf50789ae21b75f4c2468d` | Apple phase/scanner/execution evidence; format, clippy, diff, generator checks |
| #1085 | `d4443aaee75c073b61a2f14daf02b3cafd104f39` | Protected merge workflow; Apple identity/cache source review |
| #1090 | `61b6edd61d6e338bf8b7169f4d741f5b6fa06c2e` | macOS stream blocking regression; landed after review |
| #1088 | `e9b534ee1cf0ab6c833c2904be866054bad455ae` | Swift scanner/cache/watch and schema-1/S2 tests |
| #1089 | `45ba3841fe9fbbfa0cbdfcfd81d1667a9ab3e7b2` | Linux/tooling/cache and runner-probe checks; protected merge |
| #1091 | `52dc35b11aa190b62c082e70972b26c203598ef7` | Permit-ledger schema readiness and idempotent repair tests |
| #1095 | `cdee4acda4c0f4bcc01c102a9e0a4bb6a173bacb` | Published runtime-product target promotion |
| #1100 | `507e722618371256cf1d2efe5e8dec2399d66650` | Raw-store TOCTOU/quarantine checks; post-merge liveness defect recorded above |
| #1103 | `6391a0ce53b7e666d1bfb391083011622158f4f3` | Two exact-head reviews passed; PR CI `35796867457` passed |
| #1093 | `0eeac75a157da9c1e4171d56b663fe5943e85fed` | Required PR CI `35798591914` passed; PR merged through protected workflow |

### 6.2 Main and protected-CI evidence — CURRENT/OPEN/UNPROVEN

- `main` currently resolves to `0eeac75a157da9c1e4171d56b663fe5943e85fed`.
- Since the initial ledger baseline, the observed main advance was #1093's
  protected squash merge; no claim is made here about later unobserved remote
  changes.
- The post-#1103 Runtime products run observed was
  `35797774869` with success. CI run `35797775089` and Preview run
  `35797775098` were still in progress in the captured post-merge observation;
  this ledger does not convert that observation into a final all-CI claim.
- For the merged #1100 state, CI run `35791295401` and Runtime products run
  `35791295100` succeeded; Preview run `35791295272` failed because stale
  Debian artifacts left two packages in the collection directory. This is the
  evidence for the still-open #1101 isolation work, not a claim that #1101 is
  accepted.
- Open PR status is not acceptance. A successful focused test or a successful
  individual workflow does not prove integrated current-main behavior,
  required review completion, or a publishable runtime product.

## 7. Cleanup and deletion gate

Cleanup status: **OPEN/UNPROVEN**.

No repository, remote branch, local branch, worktree, stash, detached commit,
clone, tag, release, reflog, or recovery object has been deleted under this
ledger. No remote-tracking ref has been pruned. No Git garbage collection or
reflog expiry has been run.

The deletion gate remains closed because:

1. the current primary checkout and several worktrees have active writers,
   conflicts, dirty indexes, operation state, or unresolved ignored/untracked
   material;
2. the latest coherent recovery capture predates current ref and filesystem
   changes;
3. open source PRs and local families above have not all reached a final
   disposition; and
4. the final `main` CI and clean-checkout verification record is not complete.

Before any deletion, a fresh freeze capture must be made and independently
restored. Each deletion must then have an exact allowlisted identity, last-seen
SHA or state digest, unique-work disposition, verified main proof, recovery ID,
active-use check, and independent deletion review. A changed ref or filesystem
state cancels that deletion.

## 8. Retained exceptions

| ID | Retained item | Concrete purpose |
| --- | --- | --- |
| `RETAIN-MAIN` | Authoritative `main` and protected tags/releases | Development authority and release history |
| `RETAIN-PRIMARY` | Canonical primary checkout/common Git storage | Active work, dirty-state preservation, and final clean-checkout target; not currently clean or safe to remove |
| `RETAIN-ACTIVE-WORKTREES` | Worktrees with active writers, locks, conflicts, or unresolved dirty state | Prevent loss of unreviewed work; each needs independent disposition |
| `RETAIN-OPEN-SOURCES` | Open PR branches and local source families in §4.3 and §5 | Pending review, repair, or selective extraction |
| `RETAIN-RECOVERY-VELNOR-20260922` | Private recovery archive and restoration metadata | Recovery protection; not a development branch and not yet a final current-state capture |
| `RETAIN-UNRELATED` | Repositories and nested projects without Velnor identity | Outside this goal's deletion scope |

## 9. Remaining limits and next state

| Limit | Status and effect |
| --- | --- |
| Organization ruleset visibility | **CURRENT LIMIT.** HTTP 403 prevents proving organization-level ruleset completeness |
| Deleted/inaccessible historical PR source | **CURRENT LIMIT.** PR #826 cannot be fully reconstructed from the accessible remote |
| Moving remote and host state | **OPEN/UNPROVEN.** The inventory and recovery record must be refreshed immediately before cleanup |
| Raw-store retention follow-up | **OPEN/UNPROVEN.** #1106 must pass protected CI and independent exact-head review before the #1100 liveness defect is closed |
| Action scanner migration | **FUTURE/OPEN.** Contract foundation, caller migration, closure staging, and published-product promotion remain separate acceptance gates |
| Phase retention and Debian isolation | **OPEN/UNPROVEN.** Existing PRs have focused evidence but unresolved correctness findings or failed gates |
| Final clean canonical checkout | **FUTURE/OPEN.** It must be verified against the final observed remote `main`, not this ledger's current snapshot alone |

This document records an audit checkpoint. It does not claim completion,
successful cleanup, absence of bugs, or acceptance of any source not explicitly
mapped to a landed commit above.
