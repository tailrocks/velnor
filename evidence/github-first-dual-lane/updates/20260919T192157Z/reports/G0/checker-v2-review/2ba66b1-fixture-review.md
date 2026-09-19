# Exact-checkpoint hostile-fixture review

Review target: `dual-lane-checker` commit
`2ba66b116dd5511f0b4f2a6856cfbed6bd290152`, detached clean worktree.
This is an independent source/fixture review. **No approval.**

## Entrypoint and controls

The exact compiled command was:

```text
target/debug/velnor-tools evidence-check --stage <G0|G1> \
  --manifest /tmp/g2-checker-x8njgs/fixtures/<case>/manifest.json \
  --snapshot /tmp/g2-checker-x8njgs/fixtures/<case>/snapshot.json \
  --evidence /tmp/g2-checker-x8njgs/fixtures/<case>/evidence.json --json
```

Fixtures were generated from one positive 32-repository typed document, then
one property was mutated per case. `g0-positive` and `g1-positive` are positive
controls. Exit `0` plus JSON `status: pass` is the observed result; exit `1`
plus `status: fail` is a rejection.

| Case | Stage | Expected | Observed | Finding codes |
|---|---:|---|---|---|
| `g0-positive` | G0 | pass | **pass**, exit 0 | — |
| `g0-missing-inventory` | G0 | fail | fail, exit 1 | `missing-g0-inventory` |
| `g0-third-provider` | G0 | **fail** | **false green**, pass, exit 0 | — |
| `g0-blocker` (`blocker` set) | G0 | **fail** | **false green**, pass, exit 0 | — |
| `g0-ruleset-mismatch` (manifest context/App changed) | G0 | **fail** | **false green**, pass, exit 0 | — |
| `g0-job-target-mismatch` (workload remains Linux, expected job becomes macOS/arm64) | G0 | **fail** | **false green**, pass, exit 0 | — |
| `g0-pr-without-check-inventory` (one open PR, no PR check/App rows) | G0 | **fail** | **false green**, pass, exit 0 | — |
| `g0-scope-substitute` | G0 | fail | fail, exit 1 | `manifest-scope`, `missing-repository`, `missing-snapshot-repository`, `snapshot-out-of-scope`, `unknown-repository` |
| `g0-missing-workflow` | G0 | fail | fail, exit 1 | `g0-inventory-mismatch`, `missing-workflow-inventory` |
| `g0-missing-record` | G0 | fail | fail, exit 1 | `missing-repository` |
| `g1-positive` | G1 | pass | **pass**, exit 0 | — |
| `g1-empty-log` | G1 | fail | fail, exit 1 | `missing-log` |
| `g1-unrelated-log` (`https://example.invalid/unrelated`) | G1 | **fail** | **false green**, pass, exit 0 | — |
| `g1-failed-extra-job` | G1 | fail | fail, exit 1 | `job-conclusion`, `job-inventory-mismatch` |
| `g1-host-spoof` (runner name says Velnor, kind remains GitHub-hosted) | G1 | **fail** | **false green**, pass, exit 0 | — |
| `g1-wrong-url` (run/job/check URLs moved to another host/repository) | G1 | **fail** | **false green**, pass, exit 0 | — |
| `g1-unrelated-check-url` | G1 | **fail** | **false green**, pass, exit 0 | — |
| `g1-duplicate-job-id` | G1 | fail | fail, exit 1 | `duplicate-job` |
| `g1-manual-only` (`workflow_dispatch`, same SHA) | G1 | fail | fail, exit 1 | `event-source`, `missing-main-evidence` |

## Exact findings

1. **G0 early return bypasses generic terminal bookkeeping.** At
   `evidence_check.rs:2242-2245`, `check_record` calls `check_g0_record` and
   returns before the generic `gate_status`, `blocker`, and `next_action`
   checks at `:2312-2331`. `g0-blocker` therefore passes with an unresolved
   blocker. Keep G0 inventory status distinct from execution `pass`, but still
   reject blocker/next-action claims and any unresolved status.
2. **Provider eligibility is not an exact key set.** `check_eligibility`
   (`:3881-3897`) only requires `github` and `velnor`; it does not reject a
   third eligible provider. `g0-third-provider` passes. Require exactly those
   two keys; no normalization or third-provider escape hatch.
3. **Manifest required checks are not reconciled to live rulesets.** Manifest
   contexts are shape-checked, snapshot ruleset contexts are shape-checked,
   but no equality/binding check exists. `g0-ruleset-mismatch` passes.
4. **Workload→job target edge is not checked.** Supported job target syntax is
   validated, but no invariant binds each expected job's platform/architecture
   to its referenced workload row. `g0-job-target-mismatch` passes.
5. **G0 PR rows have no required-check/App inventory.** The new PR fields
   (`SnapshotPullRequest`, `:399-416`) cover draft/author/head repository and
   merge-group identity, but a PR's required checks/apps are not represented or
   required. `g0-pr-without-check-inventory` passes with an open PR and zero
   PR executions/check rows. G0 must inventory this data; G1+ must prove it.
6. **Logs are nonempty URL syntax only.** `g1-empty-log` correctly fails, but
   `g1-unrelated-log` passes. Log records need typed run/job/step identity and
   independently collected content/source binding; an arbitrary HTTPS string is
   not evidence.
7. **Runner/host binding is heuristic.** `g1-host-spoof` passes with a
   Velnor-looking runner name while claiming `github-hosted`; exact source only
   checks the declared kind/labels/host fields. Require trusted provider/runner
   registration and host capability evidence.
8. **Source URLs are not repository-bound.** `g1-wrong-url` and
   `g1-unrelated-check-url` pass after moving run/job/check URLs to an unrelated
   host. Require canonical GitHub API object IDs, repository, source SHA, and
   run/job/check associations; URL syntax is only a display link.
9. **Manual-only evidence is rejected in this checkpoint.** `g1-manual-only`
   fails because current-main coverage requires a `push` record and source
   semantics rejects the event. Retain this behavior; a manual dispatch may be
   diagnostic only and must never replace required push/PR/check association.
10. **Fixed names/count are retained.** `g0-scope-substitute` fails, so the
    exact 32-name check must remain when future schema/normalization work lands.

## Additional source-contract gaps

- `main.rs:78-80` still exposes legacy CLI aliases
  `check-evidence`, `evidence-verify`, and `verify-evidence`. The no-legacy
  migration contract requires one canonical `evidence-check` command; unknown
  old commands must fail.
- `docs/ci/github-first-dual-lane/evidence-schema.md` describes a recursive
  child graph and complete PR facts, but the exact structs still lack child
  jobs/checks/logs/descendants and PR check/App observations. Documentation is
  not implementation evidence.
- G0 positive control passes with no typed dependency graph fields because the
  current envelope only carries digest strings in `g0_inventory`. The digest
  must bind actual graph bytes and typed edges, not a self-attested digest.
- Action artifact collection remains absent. No fixture can pass an artifact
  page-2, missing-artifact, empty-log-body, or child-log completeness test
  because the exact schema/collector has no such observations. Add endpoint
  records and test first-page=100 → second-page=1, page-2 API failure, and
  pagination-cap truncation before acceptance.

The positive controls demonstrate the command can pass a synthetic complete
shape. The false greens above demonstrate that passing unit tests and a
synthetic envelope are not semantic acceptance proof.
