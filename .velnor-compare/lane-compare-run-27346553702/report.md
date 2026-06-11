# Lane compare — run 27346553702 (ChainArgos/java-monorepo)

Gate: equal-or-better — zero rows where the GitHub lane shows information the Velnor lane lacks.

## Format and lint (GitHub) ⇄ Format and lint (Velnor)

Jobs: github `80796708658` (success) ⇄ velnor `80796708609` (success)

| # | step (github) | step (velnor) | gh concl | vl concl | gh expand | vl expand | gh dur | vl dur | verdict |
|---|---------------|---------------|----------|----------|-----------|-----------|--------|--------|---------|
| 1/1 | Set up job | Set up job | success | success | ? | ? | 2s | 0s | ok |
| 2/2 | Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | success | success | ? | ? | 7s | 0s | ok |
| 3/3 | Cache Cargo registry | Cache Cargo registry | success | success | ? | ? | 6s | 0s | ok |
| 4/4 | Run rui314/setup-mold@9c9c13bf4c3f1adef0cc596abc155580bcb04444 | Run rui314/setup-mold@9c9c13bf4c3f1adef0cc596abc155580bcb04444 | success | success | ? | ? | 0s | 0s | ok |
| 5/5 | Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | success | success | ? | ? | 1s | 0s | ok |
| 6/6 | Run jdx/mise-action@dba19683ed58901619b14f395a24841710cb4925 | Run jdx/mise-action@dba19683ed58901619b14f395a24841710cb4925 | success | success | ? | ? | 6s | 1s | ok |
| 7/7 | Ensure Rust components (rustfmt, clippy) | Ensure Rust components (rustfmt, clippy) | success | success | ? | ? | 10s | 0s | ok |
| 8/8 | Rustfmt | Rustfmt | success | success | ? | ? | 1s | 0s | ok |
| 9/9 | Clippy | Clippy | success | success | ? | ? | 254s | 52s | ok |
| 16/16 | Post Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | Post Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | success | success | ? | ? | 0s | 0s | ok |
| 17/17 | Post Cache Cargo registry | Post Cache Cargo registry | success | success | ? | ? | 2s | 0s | ok |
| 18/18 | Post Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | Post Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | success | success | ? | ? | 0s | 0s | ok |
| 19/19 | Complete job | Complete job | success | success | ? | ? | 0s | 0s | ok |

Lane log content: github 1080 lines (1074 timestamped, 29 groups, ansi: true) ⇄ velnor 3386 lines (3287 timestamped, 146 groups, ansi: true)

## Test bitcoin-processor-app (GitHub) ⇄ Test bitcoin-processor-app (Velnor)

Jobs: github `80796708635` (success) ⇄ velnor `80796708615` (success)

| # | step (github) | step (velnor) | gh concl | vl concl | gh expand | vl expand | gh dur | vl dur | verdict |
|---|---------------|---------------|----------|----------|-----------|-----------|--------|--------|---------|
| 1/1 | Set up job | Set up job | success | success | ? | ? | 2s | 0s | ok |
| 2/2 | Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | success | success | ? | ? | 4s | 0s | ok |
| 3/3 | Cache Cargo registry | Cache Cargo registry | success | success | ? | ? | 5s | 0s | ok |
| 4/4 | Run rui314/setup-mold@9c9c13bf4c3f1adef0cc596abc155580bcb04444 | Run rui314/setup-mold@9c9c13bf4c3f1adef0cc596abc155580bcb04444 | success | success | ? | ? | 1s | 0s | ok |
| 5/5 | Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | success | success | ? | ? | 1s | 0s | ok |
| 6/6 | Run jdx/mise-action@dba19683ed58901619b14f395a24841710cb4925 | Run jdx/mise-action@dba19683ed58901619b14f395a24841710cb4925 | success | success | ? | ? | 5s | 1s | ok |
| 7/7 | Tests | Tests | success | success | ? | ? | 204s | 143s | ok |
| 12/12 | Post Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | Post Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | success | success | ? | ? | 0s | 0s | ok |
| 13/13 | Post Cache Cargo registry | Post Cache Cargo registry | success | success | ? | ? | 1s | 0s | ok |
| 14/14 | Post Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | Post Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | success | success | ? | ? | 0s | 0s | ok |
| 15/15 | Complete job | Complete job | success | success | ? | ? | 0s | 0s | ok |

Lane log content: github 996 lines (990 timestamped, 27 groups, ansi: true) ⇄ velnor 3386 lines (3287 timestamped, 146 groups, ansi: true)

## Test blockchain-explorer (GitHub) ⇄ Test blockchain-explorer (Velnor)

Jobs: github `80796708627` (success) ⇄ velnor `80796708583` (success)

| # | step (github) | step (velnor) | gh concl | vl concl | gh expand | vl expand | gh dur | vl dur | verdict |
|---|---------------|---------------|----------|----------|-----------|-----------|--------|--------|---------|
| 1/1 | Set up job | Set up job | success | success | ? | ? | 3s | 0s | ok |
| 2/2 | Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | success | success | ? | ? | 4s | 0s | ok |
| 3/3 | Cache Cargo registry | Cache Cargo registry | success | success | ? | ? | 6s | 0s | ok |
| 4/4 | Run rui314/setup-mold@9c9c13bf4c3f1adef0cc596abc155580bcb04444 | Run rui314/setup-mold@9c9c13bf4c3f1adef0cc596abc155580bcb04444 | success | success | ? | ? | 0s | 0s | ok |
| 5/5 | Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | success | success | ? | ? | 1s | 0s | ok |
| 6/6 | Run jdx/mise-action@dba19683ed58901619b14f395a24841710cb4925 | Run jdx/mise-action@dba19683ed58901619b14f395a24841710cb4925 | success | success | ? | ? | 4s | 2s | ok |
| 7/7 | Tests | Tests | success | success | ? | ? | 217s | 103s | ok |
| 12/12 | Post Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | Post Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | success | success | ? | ? | 0s | 0s | ok |
| 13/13 | Post Cache Cargo registry | Post Cache Cargo registry | success | success | ? | ? | 1s | 0s | ok |
| 14/14 | Post Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | Post Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | success | success | ? | ? | 1s | 0s | ok |
| 15/15 | Complete job | Complete job | success | success | ? | ? | 0s | 0s | ok |

Lane log content: github 937 lines (931 timestamped, 27 groups, ansi: true) ⇄ velnor 3386 lines (3287 timestamped, 146 groups, ansi: true)

## Test coingecko-pricing-app (GitHub) ⇄ Test coingecko-pricing-app (Velnor)

Jobs: github `80796708677` (success) ⇄ velnor `80796708603` (success)

| # | step (github) | step (velnor) | gh concl | vl concl | gh expand | vl expand | gh dur | vl dur | verdict |
|---|---------------|---------------|----------|----------|-----------|-----------|--------|--------|---------|
| 1/1 | Set up job | Set up job | success | success | ? | ? | 1s | 0s | ok |
| 2/2 | Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | success | success | ? | ? | 4s | 0s | ok |
| 3/3 | Cache Cargo registry | Cache Cargo registry | success | success | ? | ? | 6s | 0s | ok |
| 4/4 | Run rui314/setup-mold@9c9c13bf4c3f1adef0cc596abc155580bcb04444 | Run rui314/setup-mold@9c9c13bf4c3f1adef0cc596abc155580bcb04444 | success | success | ? | ? | 0s | 0s | ok |
| 5/5 | Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | success | success | ? | ? | 1s | 0s | ok |
| 6/6 | Run jdx/mise-action@dba19683ed58901619b14f395a24841710cb4925 | Run jdx/mise-action@dba19683ed58901619b14f395a24841710cb4925 | success | success | ? | ? | 3s | 1s | ok |
| 7/7 | Tests | Tests | success | success | ? | ? | 163s | 137s | ok |
| 12/12 | Post Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | Post Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | success | success | ? | ? | 0s | 0s | ok |
| 13/13 | Post Cache Cargo registry | Post Cache Cargo registry | success | success | ? | ? | 2s | 0s | ok |
| 14/14 | Post Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | Post Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | success | success | ? | ? | 0s | 0s | ok |
| 15/15 | Complete job | Complete job | success | success | ? | ? | 0s | 0s | ok |

Lane log content: github 903 lines (897 timestamped, 27 groups, ansi: true) ⇄ velnor 3386 lines (3287 timestamped, 146 groups, ansi: true)

## Test eth-grpc-server (GitHub) ⇄ Test eth-grpc-server (Velnor)

Jobs: github `80796708608` (success) ⇄ velnor `80796708691` (success)

| # | step (github) | step (velnor) | gh concl | vl concl | gh expand | vl expand | gh dur | vl dur | verdict |
|---|---------------|---------------|----------|----------|-----------|-----------|--------|--------|---------|
| 1/1 | Set up job | Set up job | success | success | ? | ? | 1s | 0s | ok |
| 2/2 | Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | success | success | ? | ? | 4s | 0s | ok |
| 3/3 | Cache Cargo registry | Cache Cargo registry | success | success | ? | ? | 6s | 0s | ok |
| 4/4 | Run rui314/setup-mold@9c9c13bf4c3f1adef0cc596abc155580bcb04444 | Run rui314/setup-mold@9c9c13bf4c3f1adef0cc596abc155580bcb04444 | success | success | ? | ? | 1s | 0s | ok |
| 5/5 | Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | success | success | ? | ? | 1s | 0s | ok |
| 6/6 | Run jdx/mise-action@dba19683ed58901619b14f395a24841710cb4925 | Run jdx/mise-action@dba19683ed58901619b14f395a24841710cb4925 | success | success | ? | ? | 5s | 1s | ok |
| 7/7 | Tests | Tests | success | success | ? | ? | 318s | 209s | ok |
| 12/12 | Post Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | Post Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | success | success | ? | ? | 1s | 0s | ok |
| 13/13 | Post Cache Cargo registry | Post Cache Cargo registry | success | success | ? | ? | 2s | 0s | ok |
| 14/14 | Post Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | Post Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | success | success | ? | ? | 0s | 0s | ok |
| 15/15 | Complete job | Complete job | success | success | ? | ? | 0s | 0s | ok |

Lane log content: github 960 lines (954 timestamped, 27 groups, ansi: true) ⇄ velnor 3386 lines (3287 timestamped, 146 groups, ansi: true)

## Test eth-processor-app (GitHub) ⇄ Test eth-processor-app (Velnor)

Jobs: github `80796708561` (success) ⇄ velnor `80796708696` (success)

| # | step (github) | step (velnor) | gh concl | vl concl | gh expand | vl expand | gh dur | vl dur | verdict |
|---|---------------|---------------|----------|----------|-----------|-----------|--------|--------|---------|
| 1/1 | Set up job | Set up job | success | success | ? | ? | 1s | 0s | ok |
| 2/2 | Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | success | success | ? | ? | 5s | 0s | ok |
| 3/3 | Cache Cargo registry | Cache Cargo registry | success | success | ? | ? | 7s | 0s | ok |
| 4/4 | Run rui314/setup-mold@9c9c13bf4c3f1adef0cc596abc155580bcb04444 | Run rui314/setup-mold@9c9c13bf4c3f1adef0cc596abc155580bcb04444 | success | success | ? | ? | 1s | 0s | ok |
| 5/5 | Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | success | success | ? | ? | 1s | 0s | ok |
| 6/6 | Run jdx/mise-action@dba19683ed58901619b14f395a24841710cb4925 | Run jdx/mise-action@dba19683ed58901619b14f395a24841710cb4925 | success | success | ? | ? | 4s | 1s | ok |
| 7/7 | Tests | Tests | success | success | ? | ? | 292s | 200s | ok |
| 12/12 | Post Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | Post Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | success | success | ? | ? | 0s | 0s | ok |
| 13/13 | Post Cache Cargo registry | Post Cache Cargo registry | success | success | ? | ? | 2s | 0s | ok |
| 14/14 | Post Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | Post Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | success | success | ? | ? | 1s | 0s | ok |
| 15/15 | Complete job | Complete job | success | success | ? | ? | 0s | 0s | ok |

Lane log content: github 1053 lines (1047 timestamped, 27 groups, ansi: true) ⇄ velnor 3386 lines (3287 timestamped, 146 groups, ansi: true)

## Test legacy-grpc-server (GitHub) ⇄ Test legacy-grpc-server (Velnor)

Jobs: github `80796708578` (success) ⇄ velnor `80796708656` (success)

| # | step (github) | step (velnor) | gh concl | vl concl | gh expand | vl expand | gh dur | vl dur | verdict |
|---|---------------|---------------|----------|----------|-----------|-----------|--------|--------|---------|
| 1/1 | Set up job | Set up job | success | success | ? | ? | 3s | 0s | ok |
| 2/2 | Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | success | success | ? | ? | 5s | 0s | ok |
| 3/3 | Cache Cargo registry | Cache Cargo registry | success | success | ? | ? | 8s | 0s | ok |
| 4/4 | Run rui314/setup-mold@9c9c13bf4c3f1adef0cc596abc155580bcb04444 | Run rui314/setup-mold@9c9c13bf4c3f1adef0cc596abc155580bcb04444 | success | success | ? | ? | 0s | 0s | ok |
| 5/5 | Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | success | success | ? | ? | 1s | 0s | ok |
| 6/6 | Run jdx/mise-action@dba19683ed58901619b14f395a24841710cb4925 | Run jdx/mise-action@dba19683ed58901619b14f395a24841710cb4925 | success | success | ? | ? | 6s | 1s | ok |
| 7/7 | Tests | Tests | success | success | ? | ? | 248s | 172s | ok |
| 12/12 | Post Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | Post Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | success | success | ? | ? | 0s | 0s | ok |
| 13/13 | Post Cache Cargo registry | Post Cache Cargo registry | success | success | ? | ? | 2s | 0s | ok |
| 14/14 | Post Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | Post Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | success | success | ? | ? | 1s | 0s | ok |
| 15/15 | Complete job | Complete job | success | success | ? | ? | 0s | 0s | ok |

Lane log content: github 947 lines (941 timestamped, 27 groups, ansi: true) ⇄ velnor 3386 lines (3287 timestamped, 146 groups, ansi: true)

## Test tron-grpc-server (GitHub) ⇄ Test tron-grpc-server (Velnor)

Jobs: github `80796708722` (success) ⇄ velnor `80796708668` (success)

| # | step (github) | step (velnor) | gh concl | vl concl | gh expand | vl expand | gh dur | vl dur | verdict |
|---|---------------|---------------|----------|----------|-----------|-----------|--------|--------|---------|
| 1/1 | Set up job | Set up job | success | success | ? | ? | 2s | 0s | ok |
| 2/2 | Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | success | success | ? | ? | 4s | 0s | ok |
| 3/3 | Cache Cargo registry | Cache Cargo registry | success | success | ? | ? | 4s | 0s | ok |
| 4/4 | Run rui314/setup-mold@9c9c13bf4c3f1adef0cc596abc155580bcb04444 | Run rui314/setup-mold@9c9c13bf4c3f1adef0cc596abc155580bcb04444 | success | success | ? | ? | 1s | 0s | ok |
| 5/5 | Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | success | success | ? | ? | 1s | 0s | ok |
| 6/6 | Run jdx/mise-action@dba19683ed58901619b14f395a24841710cb4925 | Run jdx/mise-action@dba19683ed58901619b14f395a24841710cb4925 | success | success | ? | ? | 3s | 1s | ok |
| 7/7 | Tests | Tests | success | success | ? | ? | 308s | 203s | ok |
| 12/12 | Post Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | Post Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | success | success | ? | ? | 0s | 0s | ok |
| 13/13 | Post Cache Cargo registry | Post Cache Cargo registry | success | success | ? | ? | 2s | 0s | ok |
| 14/14 | Post Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | Post Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | success | success | ? | ? | 1s | 0s | ok |
| 15/15 | Complete job | Complete job | success | success | ? | ? | 0s | 0s | ok |

Lane log content: github 953 lines (947 timestamped, 27 groups, ansi: true) ⇄ velnor 3386 lines (3287 timestamped, 146 groups, ansi: true)

## Test tron-processor-app (GitHub) ⇄ Test tron-processor-app (Velnor)

Jobs: github `80796708680` (success) ⇄ velnor `80796708753` (success)

| # | step (github) | step (velnor) | gh concl | vl concl | gh expand | vl expand | gh dur | vl dur | verdict |
|---|---------------|---------------|----------|----------|-----------|-----------|--------|--------|---------|
| 1/1 | Set up job | Set up job | success | success | ? | ? | 4s | 0s | ok |
| 2/2 | Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | success | success | ? | ? | 4s | 0s | ok |
| 3/3 | Cache Cargo registry | Cache Cargo registry | success | success | ? | ? | 7s | 0s | ok |
| 4/4 | Run rui314/setup-mold@9c9c13bf4c3f1adef0cc596abc155580bcb04444 | Run rui314/setup-mold@9c9c13bf4c3f1adef0cc596abc155580bcb04444 | success | success | ? | ? | 1s | 0s | ok |
| 5/5 | Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | success | success | ? | ? | 1s | 0s | ok |
| 6/6 | Run jdx/mise-action@dba19683ed58901619b14f395a24841710cb4925 | Run jdx/mise-action@dba19683ed58901619b14f395a24841710cb4925 | success | success | ? | ? | 6s | 1s | ok |
| 7/7 | Tests | Tests | success | success | ? | ? | 206s | 133s | ok |
| 12/12 | Post Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | Post Run mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 | success | success | ? | ? | 1s | 0s | ok |
| 13/13 | Post Cache Cargo registry | Post Cache Cargo registry | success | success | ? | ? | 1s | 0s | ok |
| 14/14 | Post Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | Post Run actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10 | success | success | ? | ? | 0s | 0s | ok |
| 15/15 | Complete job | Complete job | success | success | ? | ? | 0s | 0s | ok |

Lane log content: github 1300 lines (1294 timestamped, 27 groups, ansi: true) ⇄ velnor 3386 lines (3287 timestamped, 146 groups, ansi: true)

## Result

**PASS** — no paired step is less informative than the GitHub lane.

Known documented divergence (not gated): V2 jobs have no v1 log archive, so the per-job raw-log download 404s on the Velnor lane; the `job-log` artifact is the workaround (master-plan P4.3).
