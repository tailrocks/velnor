# Checker adversarial extension report

Date: 2026-09-20 UTC

This is a read-only second-wave regression matrix. The historical 34-case
matrix and its output were not rerun or changed. New fixture generators and
outputs are under this directory only.

## Exact anchors

- d9 baseline: `2ba66b116dd5511f0b4f2a6856cfbed6bd290152`, clean detached worktree `/private/tmp/g1-checker-adversarial-final`.
- b11 slice: `b11b57b73f0bae8b9a7edf530adf6938c83c5fa2`, clean detached worktree `/private/tmp/g2-checker-b11-review`.
- Exact b11 `cargo check -p velnor-tools`: pass.
- Exact b11 `cargo test -p velnor-tools evidence_check`: 17 passed, 207 filtered.
- Binaries were built into `/private/tmp/g1-checker-target-2ba` and `/private/tmp/g1-checker-target-b11-exact`.
- No source worktree was edited. No dispatch, API mutation, generated repository file, or remote write was used.

Commands:

```text
CARGO_TARGET_DIR=/private/tmp/g1-checker-target-2ba rtk cargo build -p velnor-tools --bin velnor-tools
CARGO_TARGET_DIR=/private/tmp/g1-checker-target-b11-exact rtk cargo check -p velnor-tools
CARGO_TARGET_DIR=/private/tmp/g1-checker-target-b11-exact rtk cargo test -p velnor-tools evidence_check
PATH=/usr/bin:/bin:/usr/sbin:/sbin run_extension_cases.sh <exact-binary> <fixtures> <out>
```

Exact result ledgers:

- `out/extension-2ba-final3/results.ndjson` (d9 exact binary; G7 is blocked before fixture validation by live-token enforcement).
- `out/extension-b11-exact-final3/results.ndjson` (b11 exact).
- `out/g7-2ba/stderr` and `out/g7-b11exact/stderr` record the G7 live-token block.

## Matrix

Results below are d9 / exact b11. They are identical for extension cases
except G0, because exact b11 has the same execution/evidence field shape here.

| Case | Result | Finding codes | Meaning |
|---|---|---|---|
| `extension-positive-g1` | pass / pass | — | Positive control. |
| `g1-two-job-positive` | pass / pass | — | Independent source plan contains required job B. |
| `g1-two-job-record-omits-b` | fail / fail | `check-context-mismatch`, `job-conclusion`, `job-inventory-mismatch` | Record cannot erase source-plan job B. |
| `g1-child-complete-positive` | pass / pass | — | Complete child graph control. |
| `g1-child-obligation-erased` | **pass / pass** | — | False green: record child obligation may be erased while manifest/graph still require it. |
| `g1-stale-self-matching` | **pass / pass** | — | False green: stale SHA is self-consistent in offline snapshot/record. |
| `g1-queued-run` | fail / fail | `run-conclusion` | Queued run cannot pass through successful jobs. |
| `g1-cancelled-run` | fail / fail | `run-conclusion` | Cancelled run cannot pass through successful jobs. |
| `g1-unknown-status` | fail / fail | `run-conclusion` | Unknown run state fails closed. |
| `g1-unknown-trust` | fail / fail | `host-trust` | Unknown runner trust fails closed. |
| `g1-workflow-run-producer` | fail / fail | `event-source`, `missing-main-evidence` | `workflow_run` is not resulting-main producer evidence. |
| `g4-no-hosted-counterpart` | fail / fail | `missing-main-evidence` | Velnor-only evidence cannot satisfy independently eligible GitHub lane. |
| `g5-no-hosted-counterpart` | fail / fail | `missing-main-evidence`, `stage-mismatch` | Same G5 result; stage field is intentionally G1-shaped source evidence. |
| `g2-applicable-release-install-waiver` | **pass / pass** | — | False green: applicable plan can waive both typed release/install objects. |
| `g1-pin-mismatch` | fail / fail | `manifest-mismatch` | Generator/runtime/config/generated/scan pins are bound to manifest. |
| `g1-pin-omission` | fail / fail | `manifest-mismatch` | Empty pin/digest fields fail closed. |
| `g1-role-coverage-missing-pr` | fail / fail | `missing-pr-evidence` | One current PR requires a separate candidate record. |
| `g3-role-coverage-missing-pr` | fail / fail | `missing-pr-evidence` | G3 cannot replace a PR candidate with one resulting-main push. |
| `g1-pr-positive` | pass / pass | — | PR head/base/merge candidate control. |
| `g1-pr-missing-postmerge` | fail / fail | `missing-merge-candidate` | Open PR without merge candidate is not qualifying. |
| `g1-pr-wrong-head` | fail / fail | `stale-sha` | Wrong PR head is rejected. |
| `g1-pr-wrong-base` | fail / fail | `stale-sha` | Wrong PR base is rejected. |
| `g1-pr-wrong-merge` | fail / fail | `source-mismatch` | Wrong tested merge is rejected. |
| `g1-unassociated-manual-success` | fail / fail | `check-conclusion`, `event-source`, `job-conclusion`, `job-mismatch`, `missing-main-evidence` | Manual same-SHA success cannot impersonate resulting-main evidence. |
| `g6-dual-positive` | fail / fail | `job-inventory-mismatch` | Checker design contradiction; see below. |
| `g6-lane-source-mismatch` | fail / fail | `job-inventory-mismatch`, `lane-parity`, `stale-sha` | Source divergence is detected, with the positive-plan contradiction also present. |
| `g6-lane-workload-mismatch` | fail / fail | `job-inventory-mismatch`, `job-mismatch`, `lane-parity`, `workload-mismatch` | Workload divergence is detected, with the same contradiction. |
| `g6-duplicate-publisher` | fail / fail | `job-inventory-mismatch`, `single-publisher` | Different producer manifest digests are detected, with the same contradiction. |
| `g6-role-coverage-missing-pr` | fail / fail | `job-inventory-mismatch`, `missing-pr-evidence` | Both lanes still need the current PR candidate; push rows do not replace it. |
| `g7-self-review` | blocked / blocked | `GITHUB_TOKEN` or `GH_TOKEN` missing | G7 forces live collection before fixture validation; no token or live call was fabricated. |

G0 comparison, run separately against the positive summary fixture:

- d9 2ba: pass, 0 findings.
- exact b11: fail, 97 findings including `g0-authoritative-proof-missing`.

The b11 G0 failure is intended: summary counts/digests are not typed
authoritative proof (`dual-lane-checker/crates/velnor-tools/src/evidence_check.rs:1708`).
The other b11 G0 codes are expected because the historical fixture predates
the new collector contract. Do not call the summary fixture a b11 positive.

## Root findings

1. Child obligation erasure is a real false green. The checker derives child
   count/specs from the manifest and compares execution child links, but does
   not compare `record.expected_jobs[*].child_workflow` against the manifest
   (`dual-lane-checker/crates/velnor-tools/src/evidence_check.rs:3283`). The
   fix belongs at the canonical source-plan/record reconciliation boundary:
   require the record's provider-filtered expected-job rows, including child
   specs, to equal the manifest before accepting child graph evidence.

2. Offline stale self-match is a real freshness gap. Main source checks bind
   execution SHAs to the supplied snapshot (`dual-lane-checker/crates/velnor-tools/src/evidence_check.rs:2829`), while live reconciliation is the only
   independent current-branch check. The fixture is therefore a valid offline
   false green, not a claim that live G7 can be bypassed.

3. Applicable release/install waiver is a real contract gap. The checker
   rejects downgrade only when manifest applicability is `required`
   (`dual-lane-checker/crates/velnor-tools/src/evidence_check.rs:3441`); the
   fixture sets the plan to `applicable` and both typed records to
   `not-applicable` with reasons, so G2 passes. The canonical contract must
   define whether `applicable` means required execution; if yes, enforce that
   invariant or reserve waiver for an explicit excluded/not-applicable state.

4. G6 currently has an internal provider-plan contradiction. Job reconciliation
   filters manifest jobs by record provider but also requires
   `record.expected_jobs.len()` to equal that provider-specific count
   (`dual-lane-checker/crates/velnor-tools/src/evidence_check.rs:3121`). Lane
   parity separately requires the full `expected_jobs` arrays to be equal
   across GitHub and Velnor (`dual-lane-checker/crates/velnor-tools/src/evidence_check.rs:2308`). A two-provider positive plan cannot satisfy both as written;
   `g6-dual-positive` fails 64 `job-inventory-mismatch` findings before the
   source/workload/publisher mutations. Fix the contract shape first, then
   rerun the negative G6 cases as clean single-purpose regressions.

5. Exact b11 does not yet have an `evidence_role` field. A separate shared
   dirty worktree had a role-aware WIP schema, but it was not used for this
   exact-SHA verdict. The role-coverage fixture here tests authoritative PR
   presence (`missing-pr-evidence`); it does not claim independent typed role
   enforcement. Re-run that subset only against a committed role-aware SHA.

## Persisted artifacts

- `generate_extension.sh`: distinct G1/G2/G4/G5/PR/pin fixtures.
- `generate_g6_cases.sh`: G6 lane/publisher and G7 self-review fixtures.
- `run_extension_cases.sh`: extension-only harness; historical 34 cases are not invoked.
- `fixtures/`: raw d9-compatible extension inputs.
- `fixtures-b11-exact/`: exact b11-compatible copy.
- `out/extension-2ba-final3/`, `out/extension-b11-exact-final3/`: machine result ledgers.

Disposition: no gate approval. False greens and the G6 checker contradiction
need owner fixes or a committed schema/source contract before integrated
recovery review.
