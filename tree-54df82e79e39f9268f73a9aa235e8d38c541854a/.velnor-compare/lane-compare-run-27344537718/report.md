# Lane compare — run 27344537718 (ChainArgos/jackin-agent-brown)

Gate: equal-or-better — zero rows where the GitHub lane shows information the Velnor lane lacks.

## validate (GitHub) ⇄ validate (Velnor)

Jobs: github `80789637053` (success) ⇄ velnor `80789636902` (success)

| # | step (github) | step (velnor) | gh concl | vl concl | gh expand | vl expand | gh dur | vl dur | verdict |
|---|---------------|---------------|----------|----------|-----------|-----------|--------|--------|---------|
| 1 | Set up job | Set up job | success | success | ? | ? | 3s | 0s | ok |
| 2 | Build hadolint/hadolint-action@2332a7b74a6de0dda2e2221d575162eba76ba5e5 | Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | success | success | ? | ? | 6s | 0s | WORSE (name 'Build hadolint/hadolint-action@2332a7b74a6de0dda2e2221d575162eba76ba5e5' vs 'Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10') |
| 3 | Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | Run docker/login-action@650006c6eb7dba73a995cc03b0b2d7f5ca915bee | success | skipped | ? | ? | 0s | 0s | WORSE (name 'Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10' vs 'Run docker/login-action@650006c6eb7dba73a995cc03b0b2d7f5ca915bee'; conclusion success vs skipped) |
| 4 | Login to Docker Hub for base image pulls | Run jackin-project/jackin-role-action@918e0958a2f695087b8e262ed90d3ed7c09c787a | success | success | ? | ? | 1s | 12s | WORSE (name 'Login to Docker Hub for base image pulls' vs 'Run jackin-project/jackin-role-action@918e0958a2f695087b8e262ed90d3ed7c09c787a') |
| 5 | Run jackin-project/jackin-role-action@918e0958a2f695087b8e262ed90d3ed7c09c787a | — | success | — | ? | — | 93s | — | WORSE (missing on velnor) |
| 8 | Post Run jackin-project/jackin-role-action@918e0958a2f695087b8e262ed90d3ed7c09c787a | Post Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | success | success | ? | ? | 2s | 0s | WORSE (name 'Post Run jackin-project/jackin-role-action@918e0958a2f695087b8e262ed90d3ed7c09c787a' vs 'Post Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10') |
| 9 | Post Login to Docker Hub for base image pulls | Complete job | success | success | ? | ? | 0s | 0s | WORSE (name 'Post Login to Docker Hub for base image pulls' vs 'Complete job') |
| 10 | Post Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | — | success | — | ? | — | 1s | — | WORSE (missing on velnor) |
| 11 | Complete job | — | success | — | ? | — | 0s | — | WORSE (missing on velnor) |

Lane log content: github 1224 lines (1214 timestamped, 46 groups, ansi: true) ⇄ velnor 242 lines (239 timestamped, 14 groups, ansi: true)

## Result

**FAIL** — 8 row(s) where the Velnor lane is less informative than the GitHub lane.

Known documented divergence (not gated): V2 jobs have no v1 log archive, so the per-job raw-log download 404s on the Velnor lane; the `job-log` artifact is the workaround (master-plan P4.3).
