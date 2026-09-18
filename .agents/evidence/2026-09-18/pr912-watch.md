# PR-912 CI watch — single snapshot

- Time (UTC): 2026-09-17 ~00:54
- Branch: docs/bastion-final-plan
- Head: 40ff66ec63fc2f8a468a6b31ea45823e9cda152a
- Overall: RED — latest CI/PR + policy runs both fail

## Latest runs on branch (limit 10)

| run id | workflow | event | started (UTC) | duration | conclusion |
|---|---|---|---|---|---|
| 35167995756 | CI / PR | pull_request | 2026-09-17T00:47:46Z | 1m1s | failure |
| 35167993091 | Velnor workflow policy | pull_request_target | 2026-09-17T00:47:43Z | 22s | failure |
| 35167481541 | CI / PR | pull_request | 2026-09-17T00:40:06Z | 26s | failure |
| 35167478901 | Velnor workflow policy | pull_request_target | 2026-09-17T00:40:03Z | 16s | failure |
| 35166756436 | CI / PR | pull_request | 2026-09-17T00:29:18Z | 4m51s | failure |
| 35166754722 | Velnor workflow policy | pull_request_target | 2026-09-17T00:29:16Z | 22s | failure |
| 35162700625 | CI / PR | pull_request | 2026-09-16T23:31:56Z | 11m48s | failure |
| 35162696863 | Velnor workflow policy | pull_request_target | 2026-09-16T23:31:53Z | 26s | success |
| 35162123862 | CI / PR | pull_request | 2026-09-16T23:24:05Z | 14m50s | cancelled |
| 35162122202 | Velnor workflow policy | pull_request_target | 2026-09-16T23:24:04Z | 26s | success |

## statusCheckRollup (current head)

- FAIL: Control / Planning (CI/PR), Policy (workflow policy), ci-required, Control / Required
- SUCCESS: DCO
- SKIPPED: everything else (~35 jobs: all Rust/Bun/Docker/Docs/OpenTofu units, Prepare Cargo, Velnor admission)

## Per-job conclusions — latest CI/PR run 35167995756

- failure: Control / Planning (exit 5 in "Set up Velnor workflow runtime")
- failure: ci-required, Control / Required (aggregators)
- skipped: all other 35 jobs (cascade — planning gate failed first)
- success: none

Planning failure detail: jq error on cached runtime manifest —
`null (null) cannot be matched, as it is not a string` at manifest.json:19
(closure 9f236b40635970a422bf41a55f1529510e42f7113883c53683d24d088f79dad3;
.products[$platform].binary lookup yields null for this runner platform).

## Per-job conclusions — latest policy run 35167993091

- failure: Policy — `FAIL generated-tree`: tree differs from render of
  velnor-workflow at pin 7341ef4bdf750c1fbe419e94fb3848c5b8dde718;
  specifically `.github/ci/.github-actions-generator-state` differs from the pinned render.
- PASS: pin-declared, pin-reachable, pin-monotonic, entrypoint-pin

## vs earlier operational_store x3 (run 35162700625, 23:31Z)

Earlier failing jobs: Rust · velnor-control / Velnor, Rust · velnor-bench / Velnor,
Rust · velnor-tools / Velnor (+ ci-required, Control / Required aggregators).

Now: those Rust jobs never run (all SKIPPED). Failure mode shifted upstream:
1. Control / Planning runtime-setup crash (jq/manifest, exit 5) blocks the whole CI/PR matrix.
2. Policy generated-tree mismatch (generator-state file vs pinned render).

So the operational_store x3 failures are currently masked, not fixed — the pipeline
fails before reaching them.
