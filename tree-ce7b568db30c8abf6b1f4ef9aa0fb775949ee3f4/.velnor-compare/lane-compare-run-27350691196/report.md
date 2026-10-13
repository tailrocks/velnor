# Lane compare — run 27350691196 (ChainArgos/jackin-agent-brown)

Gate: equal-or-better — zero rows where the GitHub lane shows information the Velnor lane lacks.

## validate (GitHub) ⇄ validate (Velnor)

Jobs: github `80811473649` (success) ⇄ velnor `80811474036` (success)

| # | step (github) | step (velnor) | gh concl | vl concl | gh expand | vl expand | gh dur | vl dur | verdict |
|---|---------------|---------------|----------|----------|-----------|-----------|--------|--------|---------|
| 1/1 | Set up job | Set up job | success | success | ? | ? | 4s | 0s | ok |
| 2/— | Build hadolint/hadolint-action@2332a7b74a6de0dda2e2221d575162eba76ba5e5 | — | success | — | ? | — | 6s | — | ok (runner-generated container prep; native adapters need no build step) |
| 3/2 | Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | success | success | ? | ? | 0s | 0s | ok |
| 4/3 | Login to Docker Hub for base image pulls | Login to Docker Hub for base image pulls | success | success | ? | ? | 1s | 1s | ok |
| 5/4 | Run jackin-project/jackin-role-action@918e0958a2f695087b8e262ed90d3ed7c09c787a | Run jackin-project/jackin-role-action@918e0958a2f695087b8e262ed90d3ed7c09c787a | success | success | ? | ? | 102s | 14s | ok |
| 8/6 | Post Run jackin-project/jackin-role-action@918e0958a2f695087b8e262ed90d3ed7c09c787a | Post Run jackin-project/jackin-role-action@918e0958a2f695087b8e262ed90d3ed7c09c787a | success | success | ? | ? | 3s | 0s | ok |
| 9/7 | Post Login to Docker Hub for base image pulls | Post Login to Docker Hub for base image pulls | success | success | ? | ? | 0s | 0s | ok |
| 10/8 | Post Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | Post Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | success | success | ? | ? | 0s | 0s | ok |
| 11/9 | Complete job | Complete job | success | success | ? | ? | 0s | 0s | ok |

Lane log content: github 1231 lines (1221 timestamped, 46 groups, ansi: true) ⇄ velnor 284 lines (281 timestamped, 18 groups, ansi: true)

## Result

**PASS** — no paired step is less informative than the GitHub lane.

Known documented divergence (not gated): V2 jobs have no v1 log archive, so the per-job raw-log download 404s on the Velnor lane; the `job-log` artifact is the workaround (master-plan P4.3).
