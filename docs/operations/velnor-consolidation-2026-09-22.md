# Velnor consolidation ledger — 2026-09-22

Status: final-ledger candidate. The consolidation is landed on remote `main`;
this sanitized record is the last project-facing change before task-created
integration environments are removed.

This ledger records source identities, decisions, landing evidence, cleanup
exceptions, and restoration instructions. Host-private paths, credentials,
tokens, and raw recovery data are intentionally excluded. The corresponding
private recovery record is identified as `velnor-consolidation-recovery-20260922`.

## Authority and governance

| Item | Evidence |
| --- | --- |
| Repository | `tailrocks/velnor`, verified from the SSH remote identity and GitHub API |
| Authoritative branch | `main` |
| Main before final scanner/lane fixes | `37e7814995995c107fc704e62096fb86c4709bea` |
| Main after final scanner/lane fixes | `816046893ef55359ee2f41903d1f1f893d59303e` |
| Initial main | `45ef1ebef769c78f45315e11a798fdaafaef4c4e` |
| Contribution workflow | Protected default branch; squash-only merge; linear history; required `DCO`, `Policy`, and `ci-required` checks; review-thread resolution required |
| Landing PRs | #1069 `37e7814995995c107fc704e62096fb86c4709bea`; #1070 `b509009a33c0492f535676e4657f474b239c563c`; #1071 `816046893ef55359ee2f41903d1f1f893d59303e` |

## Discovery coverage

Discovery covered accessible Velnor remotes and forks returned by the GitHub
API, all paginated open/closed PR records needed for reconciliation, explicit
remote branch refs, the primary Git common directory and registered worktrees,
independent Velnor clones under the development and temporary roots, detached
heads, reflogs, stashes, operation markers, dirty indexes/worktrees, and
recoverable unreachable objects. Repository identity was checked from remotes,
Git metadata, and history; directory names alone were not used.

Initial coverage facts:

- Initial remote census: `main` plus 29 non-main branches; 20 open PRs were
  reviewed. No accessible Velnor fork supplied additional actionable work.
- The primary and secondary Git common directories exposed 89 and 369
  registered worktree records respectively; missing records were checked as
  exact paths, not removed by an unbounded prune.
- Eleven stash entries, detached heads, reflogs, operation markers, and about
  1040 unreachable objects were preserved before mutation.
- Independent clones included the `dual-lane-*` Velnor workspaces, `velnor2`,
  `velnor3`, the dirty source-integration clone, and task-created integration
  worktrees. Other repositories such as `velnor-apt`, `velnor-actions`,
  `homebrew-velnor`, Jackin, Parallax, and Tablerock were identified and kept
  outside this repository's cleanup scope.
- The accessible host roots used for the scan were the development project
  roots, agent-managed workspace/recovery roots, and temporary worktree roots.
  No additional inaccessible Velnor root or remote was reported by the scan;
  private/deleted GitHub history remains unknowable and is not claimed here.

## Accepted work landed

### Consolidation PR #1069

The following logical families were reviewed against current main, selectively
adapted where needed, tested, and landed in the squash merge
`37e7814995995c107fc704e62096fb86c4709bea`:

| Source | Accepted result |
| --- | --- |
| #1054 `256c24bb` | Apple/Mise provider closure, integrated as `e6f55e30` |
| #1055 `3b0b8f37`, `2a270947` | Product receipt failure closure, integrated as `1f908e1c`, `46019b3f` |
| #1056 `fce9b563`, `32bb1dee`, `9f795b2e` | Native product-input closure, adapted as `0f5e71a0`, `1390a904`, `fd32fff8` |
| #1057 `67594a08`, `0ecec338`, `d6535337`, `70268cd5` | Scheduled action/check-profile permission and regression coverage, adapted as `cddb8a9a`, `ab7f9d9d`, `1a1a1cbf`, `f9f1864f`; conflict retained both relevant test families |
| #1050 `e43951b4`, `1bed04e3`, `b7a8fb77`, `9d519b4c`, `96b835d9` | Lossless desktop candidate evidence, integrated as `94e06a11`, `74fad4f9`, `094f851b`, `5b07dfa8`, `9a60c371` |
| #1063 `228fc58f`, `5349ec32` | Verified P962 provider/tarball producer-bind subset, integrated as `7634421a`, `094805b4` |
| `b1a69b9` | Mise fleet/runtime pin and runner fallback authority; pin-integrity passed |
| #1052 valid subset | Cache verifier `7c37f3ed`; exact bootstrap branch was not merged |

### D19 promotion PR #1070

`21f3c95ea605236eda9ed33ae515664a5f48ad8a` promoted the D19 pin to the
verified consolidation runtime and squash-merged as
`b509009a33c0492f535676e4657f474b239c563c`. The post-merge runtime product
run `35719569102` succeeded and published the immutable product
`velnor-workflow-runtime-v1-5192f313011711b7`.

### Scanner and lane reconciliation PR #1071

PR #1071 squash-merged as `816046893ef55359ee2f41903d1f1f893d59303e`.
It accepted the correct portions of #963 and independent review findings:

- Rust `include_str!` ownership now uses the nearest package root, including
  nested packages and excluded package roots, with schema-1 and schema-2
  regression tests.
- Qualified and aliased builtin include macros are recognized while lexical
  shadowing and opaque macro bodies remain safe.
- Private lane evidence uses authenticated requests, per-job artifacts, API
  pagination, and complete paired-run census checks; Velnor-only steps remain
  informational as intended.
- Strict package lint and the exact test-only compatibility wrappers were
  corrected rather than weakening lint or assertions.

The final PR head `77a8abc6b45582a4ee0b04c2ffe1befb803d5c66` had no human review
threads or unresolved comments; all required checks passed in run
`35728957511` before merge.

## Disposition of other material

| Source family | Disposition | Reason/evidence |
| --- | --- | --- |
| #962 whole branch | SUPERSEDED/REJECTED | Configured cross-compilation runner preservation is already present and tested on final main; only the justified P962 subset was ported through #1063/#1069 |
| #963 whole branch | SUPERSEDED/REJECTED | Mixed branch; correct scanner/evidence portions landed in #1071, opaque `OUT_DIR` behavior was already satisfied, and the obsolete skills-scanner proposal has no current target |
| #1052 exact branch | SUPERSEDED/REJECTED | Current-main generated/runtime contract did not support the exact bootstrap rewrite; cache-verifier subset was extracted and tested |
| #1058 | BLOCKED then closed | Incomplete phase activation had strict lint/scanner-order failures and no current candidate-authority contract |
| #1044, #1064 | BLOCKED then closed | Broad activation/policy rewrites removed or invalidated current contracts; policy/fixture authority was not proven |
| #973 | SUPERSEDED/REJECTED | Current main commit `7864f9d4` deliberately retains rolling tags when ownership cannot be proven and tests the race; accepting an orphan tag would weaken that boundary |
| #978, #979, #980 | SUPERSEDED/REJECTED | Paused mixed validation/performance drafts overlap current architecture and contain no distinct verified change ready for landing |
| #1065–#1068 | SUPERSEDED/REJECTED | Historical paused handoffs; accepted Velnor work was extracted, other-project rollout was out of scope |
| Legacy rolling-preview branch | SUPERSEDED/REJECTED | Broad stale legacy migration payload; current package-release ownership model is safer and no unique accepted behavior remained |
| Apple/scaleset integration branch | SUPERSEDED/REJECTED unless independently listed by final clone audit | Mixed pilot/evidence history; accepted native/product closure behavior is already on main, remaining broad pilot changes lack current-main integrated proof |
| Performance campaign branches | EVIDENCE-ONLY / REJECTED | Measurements and drafts were retained in recovery; no unverified campaign rewrite was promoted |

Closed PRs contain disposition comments linking accepted work to #1069/#1071
or explaining the safety/contract reason for rejection. No review thread was
deleted or resolved merely to clear a gate.

## Verification evidence

| Scope | Result |
| --- | --- |
| Recovery | `git bundle verify` passed; dirty/index/untracked/ignored snapshots and checksums were verified before mutation |
| Local formatting/lint | `mise run fmt`, `actionlint`, `pin-integrity`, `deny`, lint, production topology, and release-boundary checks passed on the integrated state |
| Local tests before #1071 | `mise run test`: 6502 passed, 5 skipped; workflow all-features: 2697 passed |
| PR #1071 focused tests | Workflow include tests 13 passed; nested ownership 2 passed; tools 354 passed; strict package clippy for `velnor-tools` and `velnor-workflow` passed |
| PR #1071 integrated workflow tests | `cargo test -p velnor-workflow --all-features`: 2703 passed across 27 suites |
| PR #1069 | All required PR checks passed; runtime product run `35717854430` succeeded and published the immutable product from source closure `5192f313...` |
| PR #1070 | All required checks passed after rerunning a single flaky control test; post-merge runtime product run `35719569102` succeeded |
| Final main | Main CI run `35729846406` succeeded; runtime product run `35729846061` succeeded; Preview run `35729846586` must be recorded after completion |
| Known baseline limitation | `mise run audit-ci` reported 130 errors on both integrated and clean baseline trees; this pre-existing policy mismatch was not hidden or weakened |

## Cleanup gate

Deletion was allowlisted by exact ref/path and re-read immediately before each
operation. Recovery was kept outside cleanup targets. Dirty or operation-marked
user workspaces were not reset, cleaned, or force-removed.

| Candidate class | Disposition |
| --- | --- |
| Closed stale remote source branches for #962/#963/#973/#1044/#1050/#1052/#1054–#1058/#1063–#1068/#978–#980 | Delete only after final branch SHA recheck and lease-protected deletion; source commits remain in recovery/main history as applicable |
| Stale performance/legacy integration refs without an active owner | Delete after independent branch audit and exact SHA recheck |
| `preserve/*` remote recovery refs | Delete only after the refreshed private bundle verifies their objects; local recovery archive is the retained recovery authority |
| Main, tags, releases, required maintenance/release refs | Retain |
| Dirty primary checkout, dirty source-integration clone, and user-authored evidence/plan material | Retain; no destructive cleanup authorized |
| Task-created baseline, consolidation, D19, post-merge, and final-ledger worktrees | Remove after final PR/ledger merge, exact status/lock/process recheck, and recovery update |
| Missing registered worktree metadata | Remove only exact records proven absent and not operation/lock-associated; retain ambiguous operation-marker records with recovery evidence |

## Recovery and restoration

The private recovery record contains the verified all-history bundle, initial
refs/reflogs/stashes/worktree metadata, staged and unstaged binary-capable
patches, index/config snapshots, and separate untracked/ignored archives. A
refreshed final bundle is checked with `git bundle verify` and SHA-256 after the
last landing.

To restore Git history in an isolated repository:

1. Create an empty repository and run `git bundle verify all-recoverable.bundle`.
2. Fetch the bundle into the isolated repository, then recreate needed refs
   from the recorded full object IDs.
3. Restore index/config and apply staged/unstaged patches separately.
4. Extract untracked and ignored archives only into the intended restored
   checkout; inspect before replacing any files.

No recovery archive is a substitute for source review. It exists to make every
rejected or retained state recoverable after cleanup.

## Completion checkpoint

This ledger becomes complete only after its PR is squash-merged, final Preview
CI is green, the final remote-main SHA is recorded in the completion report,
all deletion-manifest entries have an audited outcome, the refreshed bundle is
verified, and a clean canonical checkout of that exact main SHA passes status
and synchronization checks.
