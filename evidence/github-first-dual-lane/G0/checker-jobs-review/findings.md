# G0 checker-job/provider adversarial review

Date: 2026-09-20

## Scope and exact source

- Review worktree: `/private/tmp/g1-checker-review` (detached, clean after review).
- Exact source reviewed: `b3b6b2ef5239ff3354f504b8aeb638129fd0504b` (`feat(tools): add deterministic dual-lane evidence checker`), parent `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`.
- Checker: `crates/velnor-tools/src/evidence_check.rs`.
- Contract: `docs/ci/github-first-dual-lane/evidence-schema.md` (read in full). The bastion target spec itself ends at §9; the requested §10 was also read at `content/docs/troubleshooting.mdx:172` (Result reporting).
- No implementer source was changed. The temporary review-only integration test was added and removed in the detached worktree; final `rtk git status --short --branch` was clean.

## Commands and proof

```text
rtk git worktree add --detach /private/tmp/g1-checker-review b3b6b2ef5239ff3354f504b8aeb638129fd0504b
rtk cargo test -p velnor-tools evidence_check
  cargo test: 9 passed, 207 filtered out (1 suite, 0.01s)
rtk cargo test -p velnor-tools --test checker_adversarial_review -- --nocapture
  cargo test: 10 passed (1 suite, 0.09s)
rtk git status --short --branch
  ## HEAD (no branch)
```

The temporary test generated a 32-row manifest/snapshot/evidence envelope. Every record kept `gate_status: "pass"`; the fail-closed cases therefore prove that gate/status booleans and aggregate green claims are not trusted.

## Adversarial results

| Case | Mutation | Result | Evidence |
|---|---|---|---|
| Missing expected matrix / empty units | First record `expected_jobs`, `actual_job_ids`, and conclusions emptied | **FAIL (correct)**, `empty-workloads` (plus required-check/job mismatch) | `evidence_check.rs:1928-1938`, schema `expected_jobs` requirement at `evidence-schema.md:195-200` |
| Successful wrapper, failed child | Expected child run `9`; child link terminal `failure`; parent job remained `success` | **FAIL (correct)**, `child-run-conclusion` | `evidence_check.rs:2171-2233`; no aggregate/gate boolean overrides it |
| Unexpected skipped required job | Actual `ci` conclusion changed to `skipped`; required check remained green | **FAIL (correct)**, `job-conclusion` | `evidence_check.rs:2027-2053`; schema explicitly rejects skipped checks |
| Mismatched PR integration vs head | Snapshot PR head=`d…`, merge=`e…`; record trigger/head=`d…`, `tested_merge_sha=e…`, checkout=`d…` | **FAIL (correct)**, `source-mismatch` because checkout is not tested merge | `evidence_check.rs:1842-1909`; schema `evidence-schema.md:92-96` |
| Stale evidence tip | Record `default_branch_sha=f…`; authoritative snapshot tip stayed `a…` | **FAIL (correct)**, `stale-sha` | `evidence_check.rs:1315-1322`; timestamp-before-snapshot also emits `stale-evidence` at `1333-1344` |
| Wrong runner identity despite Velnor title | G4 records changed to provider `velnor`, expected job provider `velnor`, runner title `Velnor self-hosted title`, but `host_id=github-hosted` | **PASS (incorrect)** | Runtime identity check only tests non-empty strings at `evidence_check.rs:1774-1789`; no provider↔host/runner correlation or trusted placement proof |
| Missing PR coverage | Authoritative snapshot contained open PR #42, but records contained only valid `push` rows | **PASS (incorrect)** | `check_record_coverage` only indexes manifest/provider rows (`evidence_check.rs:1217-1257`); open PRs are examined only when a supplied record is already `pull_request` (`1842-1860`) |

## Findings / narrow responsible fixes

1. **High — provider/host identity is not a meaningful comparison.** `runner_name` and `host_id` are user-supplied record strings and are accepted when non-empty. A G4 record can claim Velnor while naming `github-hosted`, and a Velnor title does not prove placement. Add an authoritative execution-identity contract (trusted Velnor health/provisioning identity, and the hosted provider identity) to the snapshot/manifest or child-run evidence; compare provider, run/attempt, host/provisioning ID, and job identity. At minimum reject an explicitly hosted host for Velnor and a local host for GitHub, but do not treat a display title as trust. This is required by the target spec §8 host/runner correlation and schema Stage G4 host identity.

2. **High — open PR coverage is omitted.** The checker never iterates `snapshot.repositories[*].open_prs` when determining required records. Add stage-specific coverage keyed by `(repository, provider, PR number, head SHA, base SHA, tested merge SHA)` and require a matching `pull_request` record for every snapshot PR at the gates that claim PR qualification. Keep main/push coverage separate; one cannot substitute for the other. Reuse the existing `check_event_source` identity checks after coverage finds the record.

Existing checks for empty expected jobs, child terminal conclusion, skipped required jobs, PR checkout/integration binding, and stale snapshot tip are meaningful comparisons against authoritative fields and fail despite `gate_status: pass`. No implementation was attempted in this review; fixes belong in the checker/schema owner’s next commit with regression fixtures for the two false-pass cases.

