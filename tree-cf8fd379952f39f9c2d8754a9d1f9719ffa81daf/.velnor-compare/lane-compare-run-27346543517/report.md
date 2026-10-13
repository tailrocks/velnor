# Lane compare — run 27346543517 (ChainArgos/jackin-agent-brown)

Gate: equal-or-better — zero rows where the GitHub lane shows information the Velnor lane lacks.

## validate (GitHub) ⇄ validate (Velnor)

Jobs: github `80796651793` (success) ⇄ velnor `80796651809` (success)

| # | step (github) | step (velnor) | gh concl | vl concl | gh expand | vl expand | gh dur | vl dur | verdict |
|---|---------------|---------------|----------|----------|-----------|-----------|--------|--------|---------|
| 1/1 | Set up job | Set up job | success | success | ? | ? | 7s | 0s | ok |
| 2/— | Build hadolint/hadolint-action@2332a7b74a6de0dda2e2221d575162eba76ba5e5 | — | success | — | ? | — | 7s | — | ok (runner-generated container prep; native adapters need no build step) |
| 3/2 | Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | success | success | ? | ? | 0s | 0s | ok |
| 4/3 | Login to Docker Hub for base image pulls | Login to Docker Hub for base image pulls | success | success | ? | ? | 1s | 0s | ok |
| 5/4 | Run jackin-project/jackin-role-action@918e0958a2f695087b8e262ed90d3ed7c09c787a | Run jackin-project/jackin-role-action@918e0958a2f695087b8e262ed90d3ed7c09c787a | success | success | ? | ? | 124s | 14s | ok |
| 8/— | Post Run jackin-project/jackin-role-action@918e0958a2f695087b8e262ed90d3ed7c09c787a | — | success | — | ? | — | 2s | — | WORSE (missing on velnor) |
| 9/— | Post Login to Docker Hub for base image pulls | — | success | — | ? | — | 1s | — | WORSE (missing on velnor) |
| 10/8 | Post Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | Post Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | success | success | ? | ? | 0s | 0s | ok |
| 11/9 | Complete job | Complete job | success | success | ? | ? | 0s | 0s | ok |

Lane log content: github 1332 lines (1322 timestamped, 46 groups, ansi: true) ⇄ velnor 263 lines (260 timestamped, 15 groups, ansi: true)

## Result

**FAIL** — 2 row(s) where the Velnor lane is less informative than the GitHub lane.

Known documented divergence (not gated): V2 jobs have no v1 log archive, so the per-job raw-log download 404s on the Velnor lane; the `job-log` artifact is the workaround (master-plan P4.3).
