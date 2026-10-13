# Retained CI failure inventory — 2026-09-20

Run/attempt metadata complete; detailed job/log sweep bounded and incomplete. All failures are NOT disposed.

- Retained runs: 7594, 76 unfiltered pages; API total_count reconciles exactly. Oldest 2026-06-04T22:42:50Z; newest 2026-09-20T16:23:53Z. Enumeration completed before 2026-09-20T16:31:45Z. Latest observed creation time is not a cutoff guarantee; refresh before integration/completion.
- Latest conclusions: {None: 1, 'failure': 1706, 'success': 3956, 'cancelled': 1812, 'startup_failure': 25, 'skipped': 94}.
- All 268 earlier attempts retrieved across 182 rerun runs: {'cancelled': 155, 'failure': 105, 'startup_failure': 1, 'success': 5, None: 2}. 30 failed earlier attempts belong to currently successful runs. Two old attempt records report queued although later attempts exist; historical API snapshots do not prove live work.
- Jobs: filter=all, per_page100, --paginate --slurp for 559 runs; 17961 unique jobs. Failed/nonterminal/cancelled job rows: 1651. Log API errors: 43.
- Main-push first-attempt green: 766/1565 terminal workflow runs (48.945687%); latest green: 770/1565. Cancellations and historical workflows included. Denominator counts workflow runs, NOT per-commit aggregate pipelines. 21 main-push runs retried. No inference of 99.9999% reliability justified.

## Collection semantics

[Workflow runs API](https://docs.github.com/en/rest/actions/workflow-runs): filtered searches cap at 1000; unfiltered all-pages enumeration returned 7594. Earlier attempts requested via /runs/ID/attempts/N. [Workflow jobs API](https://docs.github.com/en/rest/actions/workflow-jobs): default filter=latest hides prior jobs; filter=all and all pages explicitly collected.

Initial gh logs failed terminal-escape validation; --allow-escape-sequences recovered both user targets. Older missing job logs return HTTP404 or HTTP410, exact stderr retained under /tmp/velnor-failures/logs/*.err. Missing evidence is not a solved defect.

## Confirmed classes

| Class | Evidence | Disposition / required prevention |
| --- | --- | --- |
| Renderer parity | Target 35520255573/job106102991421 main845d474; Preview35522583904/job106109107399 main97bac4c | generated-tree differs pin38dbf85; current s2/policy.rs448 admits Candidate on PR, rejects mainline. Exact pin/render + source-bound bootstrap parity required. |
| Candidate acquisition | Main35520255650/job106102994568 | 15minute PR-only lookup at merge SHA finds no product. Build-once candidate must precede planning and use eligible current event. |
| Clippy dead-code | Target35520875130/job106104648299, PR977 headb3f9f5d; checked-out merge0fdb6be8 | GuardError::Contended at permit_guard.rs474; fixed7308307b; PR successor35522028476 green. Overall cancelled run still contains genuine failed job. No target formatting failure. |
| Dirty preview source | Main35515840346/job106093233951,386a5b63 | Download to workspace/metadata dirties identity-sensitive source.57e7cafc moves to runner.temp; source repair present, successor Preview blocked at policy. |
| ARM compiler route | Same run/job106093233952 | aarch64-linux-gnu-gcc missing. Current ARM Debian row still ubuntu-24.04 x64. Open PR962 relevant. Require native/cross compiler policy + PR package preflight. |
| Diagnostic lifecycle | PR35520025700/job106102422435,7a94393b | scaleset_daemon::crash_with_dead_workers_fails_explicitly lacks runner.log. Historical tested merge b9f7e19f called release_owned_state before the unchanged assertion; final selective integration excludes that lifecycle code. Source comparison proves removal; same test PASS in35522028476/job106107688847. |
| Protocol expectation | PR35521293443/job106105813547,7308307b | RunnerNotFound vs NotFound fixedc24be56e; PR successor35522028476 green. |

## Artifacts / remaining gaps

retained-runs-attempts-20260920.json.gz contains complete compact run/attempt metadata. retained-nongreen-attempts.csv covers every non-success latest/earlier attempt. inspected-failed-jobs.csv preserves individual signatures/root/prevention/disposition and raw hashes; auto classes are labels, not completed diagnosis. failure-excerpts.md.gz preserves exact excerpts. Full originals remain /tmp/velnor-failures.

Remaining: older jobs beyond Sept20+rerun sweep; cancellation intent; startup annotations (sample suite has zero check runs); unexpected skip applicability; open PR checks; independent review/regression/successor proof for every class; refresh during integration.
