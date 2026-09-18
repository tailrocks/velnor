# PR-912 CI watcher — snapshot 2 (2026-09-17 ~01:1xZ)

Branch: docs/bastion-final-plan. Read-only snapshot, no polling.

## Head position (note: premise partially wrong)

- Remote PR head (what CI sees): `1d3eb22e` (updated 2026-09-17T01:05:43Z)
- Local branch: `0c69cd06`, **ahead 2, unpushed** (`966f3623` d1-scaleset + `0c69cd06`
  merge = batches 4+5). CI has NOT run on batches 4+5 — no remote runs exist
  past 1d3eb22e. Plus 1 modified file locally (generator-state).
- Conclusion: CI state below reflects 1d3eb22e, not batches 4+5.

## Status check rollup (PR head 1d3eb22e)

- FAILURE: Control / Planning, Policy, ci-required, Control / Required
- SKIPPED: everything downstream (all Rust/Bun/Docker/Docs/OpenTofu jobs)
- SUCCESS: DCO

## Recent runs (branch docs/bastion-final-plan, limit 6)

| run | workflow | head | conclusion | event |
|-----|----------|------|------------|-------|
| 35169205376 | CI / PR | 1d3eb22e | failure | pull_request |
| 35169203933 | Velnor workflow policy | 1d3eb22e | failure | pull_request_target |
| 35168832125 | CI / PR | 89773efc | failure | pull_request |
| 35168829794 | Velnor workflow policy | 89773efc | failure | pull_request_target |
| 35167995756 | CI / PR | 40ff66ec | failure | pull_request |
| 35167993091 | Velnor workflow policy | 40ff66ec | failure | pull_request_target |

## Latest-run failure modes vs /tmp/a1-pr912-red.md

CI/PR 35169205376 — Control / Planning: **MATCHES F1 exactly**
- Same jq error, same manifest line, same cached closure:
  `jq: error (at .../9f236b40635970a422bf41a55f1529510e42f7113883c53683d24d088f79dad3/manifest.json:19): null (null) cannot be matched, as it is not a string`
- Same verify filter with `(.revision | test("^[0-9a-f]{40}$"))`.
- Verdict: UNCHANGED (Planning jq exit 5, missing-revision null).

Policy 35169203933 — Policy: **MATCHES F2 exactly**
- `FAIL generated-tree ... at 7341ef4bdf750c1fbe419e94fb3848c5b8dde718`
- Same 8 files: generator-state, project.toml, ci-main, ci-pr,
  ci-runtime-products, ci-unit-rust, preview, release.
- `VELNOR_WORKFLOW_CANDIDATE_MANIFEST` empty → candidate exception not engaged.
- ("shares the base closure" acquire line lives in a passing step, outside
  --log-failed scope; empty candidate manifest confirms the same state.)
- Verdict: UNCHANGED (generated-tree, no candidate).

## Bottom line

RED, same two failure modes as snapshot 1 (F1 + F2). No CI signal on batches 4+5
until the local ahead-2 commits are pushed.
