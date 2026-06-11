# Lane compare — run 27344544248 (ChainArgos/java-monorepo)

Gate: equal-or-better — zero rows where the GitHub lane shows information the Velnor lane lacks.

## Format and lint (GitHub) ⇄ Format and lint (Velnor)

Jobs: github `80789668577` (success) ⇄ velnor `80789668601` (success)

| # | step (github) | step (velnor) | gh concl | vl concl | gh expand | vl expand | gh dur | vl dur | verdict |
|---|---------------|---------------|----------|----------|-----------|-----------|--------|--------|---------|
| 1/1 | Set up job | Set up job | success | success | ? | ? | 4s | 0s | ok |
| 2/2 | Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | success | success | ? | ? | 5s | 0s | ok |
| 3/— | Cache Cargo registry | — | success | — | ? | — | 9s | — | WORSE (missing on velnor) |
| —/3 | — | Run actions/cache@27d5ce7f107fe9357f9df03efb73ab90386fccae | — | success | — | ? | — | 0s | velnor-only |
| 4/4 | Run rui314/setup-mold@9c9c13bf4c3f1adef0cc596abc155580bcb04444 | Run rui314/setup-mold@9c9c13bf4c3f1adef0cc596abc155580bcb04444 | success | success | ? | ? | 1s | 0s | ok |
| 5/5 | Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | success | success | ? | ? | 1s | 0s | ok |
| 6/6 | Run jdx/mise-action@dba19683ed58901619b14f395a24841710cb4925 | Run jdx/mise-action@dba19683ed58901619b14f395a24841710cb4925 | success | success | ? | ? | 6s | 0s | ok |
| 7/7 | Ensure Rust components (rustfmt, clippy) | Ensure Rust components (rustfmt, clippy) | success | success | ? | ? | 10s | 1s | ok |
| 8/8 | Rustfmt | Rustfmt | success | success | ? | ? | 1s | 0s | ok |
| 9/9 | Clippy | Clippy | success | success | ? | ? | 284s | 28s | ok |
| 16/16 | Post Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | Post Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | success | success | ? | ? | 0s | 0s | ok |
| 17/— | Post Cache Cargo registry | — | success | — | ? | — | 12s | — | WORSE (missing on velnor) |
| —/17 | — | Post Run actions/cache@27d5ce7f107fe9357f9df03efb73ab90386fccae | — | success | — | ? | — | 0s | velnor-only |
| 18/18 | Post Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | Post Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | success | success | ? | ? | 0s | 0s | ok |
| 19/19 | Complete job | Complete job | success | success | ? | ? | 0s | 0s | ok |

Lane log content: github 1060 lines (1054 timestamped, 29 groups, ansi: true) ⇄ velnor 3310 lines (3211 timestamped, 146 groups, ansi: true)

## Test bitcoin-processor-app (GitHub) ⇄ Test bitcoin-processor-app (Velnor)

Jobs: github `80789668605` (success) ⇄ velnor `80789668555` (success)

| # | step (github) | step (velnor) | gh concl | vl concl | gh expand | vl expand | gh dur | vl dur | verdict |
|---|---------------|---------------|----------|----------|-----------|-----------|--------|--------|---------|
| 1/1 | Set up job | Set up job | success | success | ? | ? | 2s | 0s | ok |
| 2/2 | Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | success | success | ? | ? | 5s | 0s | ok |
| 3/— | Cache Cargo registry | — | success | — | ? | — | 7s | — | WORSE (missing on velnor) |
| —/3 | — | Run actions/cache@27d5ce7f107fe9357f9df03efb73ab90386fccae | — | success | — | ? | — | 0s | velnor-only |
| 4/4 | Run rui314/setup-mold@9c9c13bf4c3f1adef0cc596abc155580bcb04444 | Run rui314/setup-mold@9c9c13bf4c3f1adef0cc596abc155580bcb04444 | success | success | ? | ? | 0s | 0s | ok |
| 5/5 | Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | success | success | ? | ? | 1s | 0s | ok |
| 6/6 | Run jdx/mise-action@dba19683ed58901619b14f395a24841710cb4925 | Run jdx/mise-action@dba19683ed58901619b14f395a24841710cb4925 | success | success | ? | ? | 4s | 1s | ok |
| 7/7 | Tests | Tests | success | success | ? | ? | 193s | 110s | ok |
| 12/12 | Post Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | Post Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | success | success | ? | ? | 1s | 0s | ok |
| 13/— | Post Cache Cargo registry | — | success | — | ? | — | 11s | — | WORSE (missing on velnor) |
| —/13 | — | Post Run actions/cache@27d5ce7f107fe9357f9df03efb73ab90386fccae | — | success | — | ? | — | 0s | velnor-only |
| 14/14 | Post Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | Post Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | success | success | ? | ? | 1s | 0s | ok |
| 15/15 | Complete job | Complete job | success | success | ? | ? | 0s | 0s | ok |

Lane log content: github 997 lines (991 timestamped, 27 groups, ansi: true) ⇄ velnor 3310 lines (3211 timestamped, 146 groups, ansi: true)

## Test blockchain-explorer (GitHub) ⇄ Test blockchain-explorer (Velnor)

Jobs: github `80789668543` (success) ⇄ velnor `80789668544` (success)

| # | step (github) | step (velnor) | gh concl | vl concl | gh expand | vl expand | gh dur | vl dur | verdict |
|---|---------------|---------------|----------|----------|-----------|-----------|--------|--------|---------|
| 1/1 | Set up job | Set up job | success | success | ? | ? | 4s | 0s | ok |
| 2/2 | Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | success | success | ? | ? | 5s | 0s | ok |
| 3/— | Cache Cargo registry | — | success | — | ? | — | 8s | — | WORSE (missing on velnor) |
| —/3 | — | Run actions/cache@27d5ce7f107fe9357f9df03efb73ab90386fccae | — | success | — | ? | — | 0s | velnor-only |
| 4/4 | Run rui314/setup-mold@9c9c13bf4c3f1adef0cc596abc155580bcb04444 | Run rui314/setup-mold@9c9c13bf4c3f1adef0cc596abc155580bcb04444 | success | success | ? | ? | 0s | 0s | ok |
| 5/5 | Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | success | success | ? | ? | 1s | 0s | ok |
| 6/6 | Run jdx/mise-action@dba19683ed58901619b14f395a24841710cb4925 | Run jdx/mise-action@dba19683ed58901619b14f395a24841710cb4925 | success | success | ? | ? | 7s | 0s | ok |
| 7/7 | Tests | Tests | success | success | ? | ? | 257s | 72s | ok |
| 12/12 | Post Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | Post Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | success | success | ? | ? | 1s | 0s | ok |
| 13/— | Post Cache Cargo registry | — | success | — | ? | — | 12s | — | WORSE (missing on velnor) |
| —/13 | — | Post Run actions/cache@27d5ce7f107fe9357f9df03efb73ab90386fccae | — | success | — | ? | — | 0s | velnor-only |
| 14/14 | Post Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | Post Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | success | success | ? | ? | 0s | 0s | ok |
| 15/15 | Complete job | Complete job | success | success | ? | ? | 0s | 0s | ok |

Lane log content: github 940 lines (934 timestamped, 27 groups, ansi: true) ⇄ velnor 3310 lines (3211 timestamped, 146 groups, ansi: true)

## Test coingecko-pricing-app (GitHub) ⇄ Test coingecko-pricing-app (Velnor)

Jobs: github `80789668565` (success) ⇄ velnor `80789668561` (success)

| # | step (github) | step (velnor) | gh concl | vl concl | gh expand | vl expand | gh dur | vl dur | verdict |
|---|---------------|---------------|----------|----------|-----------|-----------|--------|--------|---------|
| 1/1 | Set up job | Set up job | success | success | ? | ? | 2s | 0s | ok |
| 2/2 | Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | success | success | ? | ? | 4s | 0s | ok |
| 3/— | Cache Cargo registry | — | success | — | ? | — | 5s | — | WORSE (missing on velnor) |
| —/3 | — | Run actions/cache@27d5ce7f107fe9357f9df03efb73ab90386fccae | — | success | — | ? | — | 0s | velnor-only |
| 4/4 | Run rui314/setup-mold@9c9c13bf4c3f1adef0cc596abc155580bcb04444 | Run rui314/setup-mold@9c9c13bf4c3f1adef0cc596abc155580bcb04444 | success | success | ? | ? | 1s | 0s | ok |
| 5/5 | Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | success | success | ? | ? | 0s | 0s | ok |
| 6/6 | Run jdx/mise-action@dba19683ed58901619b14f395a24841710cb4925 | Run jdx/mise-action@dba19683ed58901619b14f395a24841710cb4925 | success | success | ? | ? | 5s | 2s | ok |
| 7/7 | Tests | Tests | success | success | ? | ? | 188s | 107s | ok |
| 12/12 | Post Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | Post Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | success | success | ? | ? | 0s | 0s | ok |
| 13/— | Post Cache Cargo registry | — | success | — | ? | — | 4s | — | WORSE (missing on velnor) |
| —/13 | — | Post Run actions/cache@27d5ce7f107fe9357f9df03efb73ab90386fccae | — | success | — | ? | — | 0s | velnor-only |
| 14/14 | Post Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | Post Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | success | success | ? | ? | 1s | 0s | ok |
| 15/15 | Complete job | Complete job | success | success | ? | ? | 0s | 0s | ok |

Lane log content: github 905 lines (899 timestamped, 27 groups, ansi: true) ⇄ velnor 3310 lines (3211 timestamped, 146 groups, ansi: true)

## Test eth-grpc-server (GitHub) ⇄ Test eth-grpc-server (Velnor)

Jobs: github `80789668680` (success) ⇄ velnor `80789668665` (success)

| # | step (github) | step (velnor) | gh concl | vl concl | gh expand | vl expand | gh dur | vl dur | verdict |
|---|---------------|---------------|----------|----------|-----------|-----------|--------|--------|---------|
| 1/1 | Set up job | Set up job | success | success | ? | ? | 1s | 0s | ok |
| 2/2 | Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | success | success | ? | ? | 5s | 0s | ok |
| 3/— | Cache Cargo registry | — | success | — | ? | — | 6s | — | WORSE (missing on velnor) |
| —/3 | — | Run actions/cache@27d5ce7f107fe9357f9df03efb73ab90386fccae | — | success | — | ? | — | 0s | velnor-only |
| 4/4 | Run rui314/setup-mold@9c9c13bf4c3f1adef0cc596abc155580bcb04444 | Run rui314/setup-mold@9c9c13bf4c3f1adef0cc596abc155580bcb04444 | success | success | ? | ? | 1s | 0s | ok |
| 5/5 | Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | success | success | ? | ? | 1s | 0s | ok |
| 6/6 | Run jdx/mise-action@dba19683ed58901619b14f395a24841710cb4925 | Run jdx/mise-action@dba19683ed58901619b14f395a24841710cb4925 | success | success | ? | ? | 4s | 2s | ok |
| 7/7 | Tests | Tests | success | success | ? | ? | 312s | 182s | ok |
| 12/12 | Post Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | Post Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | success | success | ? | ? | 0s | 1s | ok |
| 13/— | Post Cache Cargo registry | — | success | — | ? | — | 16s | — | WORSE (missing on velnor) |
| —/13 | — | Post Run actions/cache@27d5ce7f107fe9357f9df03efb73ab90386fccae | — | success | — | ? | — | 0s | velnor-only |
| 14/14 | Post Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | Post Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | success | success | ? | ? | 0s | 0s | ok |
| 15/15 | Complete job | Complete job | success | success | ? | ? | 1s | 0s | ok |

Lane log content: github 961 lines (955 timestamped, 27 groups, ansi: true) ⇄ velnor 3310 lines (3211 timestamped, 146 groups, ansi: true)

## Test eth-processor-app (GitHub) ⇄ Test eth-processor-app (Velnor)

Jobs: github `80789668606` (success) ⇄ velnor `80789668630` (success)

| # | step (github) | step (velnor) | gh concl | vl concl | gh expand | vl expand | gh dur | vl dur | verdict |
|---|---------------|---------------|----------|----------|-----------|-----------|--------|--------|---------|
| 1/1 | Set up job | Set up job | success | success | ? | ? | 5s | 0s | ok |
| 2/2 | Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | success | success | ? | ? | 4s | 0s | ok |
| 3/— | Cache Cargo registry | — | success | — | ? | — | 10s | — | WORSE (missing on velnor) |
| —/3 | — | Run actions/cache@27d5ce7f107fe9357f9df03efb73ab90386fccae | — | success | — | ? | — | 0s | velnor-only |
| 4/4 | Run rui314/setup-mold@9c9c13bf4c3f1adef0cc596abc155580bcb04444 | Run rui314/setup-mold@9c9c13bf4c3f1adef0cc596abc155580bcb04444 | success | success | ? | ? | 1s | 0s | ok |
| 5/5 | Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | success | success | ? | ? | 1s | 0s | ok |
| 6/6 | Run jdx/mise-action@dba19683ed58901619b14f395a24841710cb4925 | Run jdx/mise-action@dba19683ed58901619b14f395a24841710cb4925 | success | success | ? | ? | 6s | 4s | ok |
| 7/7 | Tests | Tests | success | success | ? | ? | 343s | 162s | ok |
| 12/12 | Post Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | Post Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | success | success | ? | ? | 0s | 0s | ok |
| 13/— | Post Cache Cargo registry | — | success | — | ? | — | 15s | — | WORSE (missing on velnor) |
| —/13 | — | Post Run actions/cache@27d5ce7f107fe9357f9df03efb73ab90386fccae | — | success | — | ? | — | 0s | velnor-only |
| 14/14 | Post Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | Post Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | success | success | ? | ? | 0s | 0s | ok |
| 15/15 | Complete job | Complete job | success | success | ? | ? | 1s | 0s | ok |

Lane log content: github 1041 lines (1035 timestamped, 27 groups, ansi: true) ⇄ velnor 3310 lines (3211 timestamped, 146 groups, ansi: true)

## Test legacy-grpc-server (GitHub) ⇄ Test legacy-grpc-server (Velnor)

Jobs: github `80789668698` (success) ⇄ velnor `80789668673` (success)

| # | step (github) | step (velnor) | gh concl | vl concl | gh expand | vl expand | gh dur | vl dur | verdict |
|---|---------------|---------------|----------|----------|-----------|-----------|--------|--------|---------|
| 1/1 | Set up job | Set up job | success | success | ? | ? | 4s | 0s | ok |
| 2/2 | Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | success | success | ? | ? | 4s | 0s | ok |
| 3/— | Cache Cargo registry | — | success | — | ? | — | 7s | — | WORSE (missing on velnor) |
| —/3 | — | Run actions/cache@27d5ce7f107fe9357f9df03efb73ab90386fccae | — | success | — | ? | — | 0s | velnor-only |
| 4/4 | Run rui314/setup-mold@9c9c13bf4c3f1adef0cc596abc155580bcb04444 | Run rui314/setup-mold@9c9c13bf4c3f1adef0cc596abc155580bcb04444 | success | success | ? | ? | 1s | 0s | ok |
| 5/5 | Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | success | success | ? | ? | 2s | 0s | ok |
| 6/6 | Run jdx/mise-action@dba19683ed58901619b14f395a24841710cb4925 | Run jdx/mise-action@dba19683ed58901619b14f395a24841710cb4925 | success | success | ? | ? | 6s | 0s | ok |
| 7/7 | Tests | Tests | success | success | ? | ? | 279s | 141s | ok |
| 12/12 | Post Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | Post Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | success | success | ? | ? | 0s | 1s | ok |
| 13/— | Post Cache Cargo registry | — | success | — | ? | — | 14s | — | WORSE (missing on velnor) |
| —/13 | — | Post Run actions/cache@27d5ce7f107fe9357f9df03efb73ab90386fccae | — | success | — | ? | — | 0s | velnor-only |
| 14/14 | Post Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | Post Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | success | success | ? | ? | 1s | 0s | ok |
| 15/15 | Complete job | Complete job | success | success | ? | ? | 0s | 0s | ok |

Lane log content: github 948 lines (942 timestamped, 27 groups, ansi: true) ⇄ velnor 3310 lines (3211 timestamped, 146 groups, ansi: true)

## Test tron-grpc-server (GitHub) ⇄ Test tron-grpc-server (Velnor)

Jobs: github `80789668719` (success) ⇄ velnor `80789668664` (success)

| # | step (github) | step (velnor) | gh concl | vl concl | gh expand | vl expand | gh dur | vl dur | verdict |
|---|---------------|---------------|----------|----------|-----------|-----------|--------|--------|---------|
| 1/1 | Set up job | Set up job | success | success | ? | ? | 3s | 0s | ok |
| 2/2 | Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | success | success | ? | ? | 7s | 0s | ok |
| 3/— | Cache Cargo registry | — | success | — | ? | — | 8s | — | WORSE (missing on velnor) |
| —/3 | — | Run actions/cache@27d5ce7f107fe9357f9df03efb73ab90386fccae | — | success | — | ? | — | 0s | velnor-only |
| 4/4 | Run rui314/setup-mold@9c9c13bf4c3f1adef0cc596abc155580bcb04444 | Run rui314/setup-mold@9c9c13bf4c3f1adef0cc596abc155580bcb04444 | success | success | ? | ? | 0s | 0s | ok |
| 5/5 | Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | success | success | ? | ? | 1s | 0s | ok |
| 6/6 | Run jdx/mise-action@dba19683ed58901619b14f395a24841710cb4925 | Run jdx/mise-action@dba19683ed58901619b14f395a24841710cb4925 | success | success | ? | ? | 6s | 0s | ok |
| 7/7 | Tests | Tests | success | success | ? | ? | 332s | 176s | ok |
| 12/12 | Post Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | Post Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | success | success | ? | ? | 1s | 0s | ok |
| 13/— | Post Cache Cargo registry | — | success | — | ? | — | 14s | — | WORSE (missing on velnor) |
| —/13 | — | Post Run actions/cache@27d5ce7f107fe9357f9df03efb73ab90386fccae | — | success | — | ? | — | 1s | velnor-only |
| 14/14 | Post Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | Post Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | success | success | ? | ? | 1s | 0s | ok |
| 15/15 | Complete job | Complete job | success | success | ? | ? | 0s | 0s | ok |

Lane log content: github 982 lines (976 timestamped, 27 groups, ansi: true) ⇄ velnor 3310 lines (3211 timestamped, 146 groups, ansi: true)

## Test tron-processor-app (GitHub) ⇄ Test tron-processor-app (Velnor)

Jobs: github `80789668575` (success) ⇄ velnor `80789668585` (success)

| # | step (github) | step (velnor) | gh concl | vl concl | gh expand | vl expand | gh dur | vl dur | verdict |
|---|---------------|---------------|----------|----------|-----------|-----------|--------|--------|---------|
| 1/1 | Set up job | Set up job | success | success | ? | ? | 2s | 0s | ok |
| 2/2 | Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | success | success | ? | ? | 4s | 0s | ok |
| 3/— | Cache Cargo registry | — | success | — | ? | — | 6s | — | WORSE (missing on velnor) |
| —/3 | — | Run actions/cache@27d5ce7f107fe9357f9df03efb73ab90386fccae | — | success | — | ? | — | 0s | velnor-only |
| 4/4 | Run rui314/setup-mold@9c9c13bf4c3f1adef0cc596abc155580bcb04444 | Run rui314/setup-mold@9c9c13bf4c3f1adef0cc596abc155580bcb04444 | success | success | ? | ? | 1s | 1s | ok |
| 5/5 | Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | success | success | ? | ? | 1s | 0s | ok |
| 6/6 | Run jdx/mise-action@dba19683ed58901619b14f395a24841710cb4925 | Run jdx/mise-action@dba19683ed58901619b14f395a24841710cb4925 | success | success | ? | ? | 6s | 0s | ok |
| 7/7 | Tests | Tests | success | success | ? | ? | 201s | 106s | ok |
| 12/12 | Post Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | Post Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | success | success | ? | ? | 0s | 0s | ok |
| 13/— | Post Cache Cargo registry | — | success | — | ? | — | 9s | — | WORSE (missing on velnor) |
| —/13 | — | Post Run actions/cache@27d5ce7f107fe9357f9df03efb73ab90386fccae | — | success | — | ? | — | 0s | velnor-only |
| 14/14 | Post Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | Post Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | success | success | ? | ? | 0s | 0s | ok |
| 15/15 | Complete job | Complete job | success | success | ? | ? | 0s | 0s | ok |

Lane log content: github 1300 lines (1294 timestamped, 27 groups, ansi: true) ⇄ velnor 3310 lines (3211 timestamped, 146 groups, ansi: true)

## Result

**FAIL** — 18 row(s) where the Velnor lane is less informative than the GitHub lane.

Known documented divergence (not gated): V2 jobs have no v1 log archive, so the per-job raw-log download 404s on the Velnor lane; the `job-log` artifact is the workaround (master-plan P4.3).
