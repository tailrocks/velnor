# Lane compare — run 29636145660 (tailrocks/velnor-actions-fixture)

Gate: equal-or-better — zero rows where the GitHub lane shows information the Velnor lane lacks.

## §2.11 Velnor budget (D)

| Job | Wall | Budget | Verdict |
|---|---:|---:|---|
| Cache off (Velnor) | 1s | 60s | PASS |
| Cache sccache (Velnor) | 3s | 60s | PASS |
| compat (app-a, Velnor, self-hosted, velnor-target-mvp, true) | 13s | 60s | PASS |
| compat (app-b, Velnor, self-hosted, velnor-target-mvp, true) | 13s | 60s | PASS |
| Postgres service (Velnor) | 22s | 60s | PASS |

Pickup SLO is reported from Velnor's versioned `job-timing` forensic records; the GitHub jobs API does not expose an authoritative broker-message timestamp.

## Cache off (GitHub) ⇄ Cache off (Velnor)

Jobs: github `88058728894` (success) ⇄ velnor `88058729116` (success)

| # | step (github) | step (velnor) | gh concl | vl concl | gh expand | vl expand | gh dur | vl dur | verdict |
|---|---------------|---------------|----------|----------|-----------|-----------|--------|--------|---------|
| 1/1 | Set up job | Set up job | success | success | yes | yes | 1s | 0s | ok |
| 2/2 | Run actions/checkout@v6 | Run actions/checkout@v6 | success | success | yes | yes | 1s | 0s | ok |
| 3/3 | Run jdx/mise-action@v4 | Run jdx/mise-action@v4 | success | success | yes | yes | 14s | 0s | ok |
| 4/4 | Run test -z "${RUSTC_WRAPPER:-}" | Run test -z "${RUSTC_WRAPPER:-}" | success | success | yes | yes | 0s | 0s | ok |
| 5/5 | Run cargo check -p app-a --locked | Run cargo check -p app-a --locked | success | success | yes | yes | 0s | 0s | ok |
| 10/10 | Post Run actions/checkout@v6 | Post Run actions/checkout@v6 | success | success | yes | yes | 1s | 0s | ok |
| 11/11 | Complete job | Complete job | success | success | yes | yes | 0s | 0s | ok |

Lane log content: github 278 lines (277 timestamped, 24 groups, ansi: true) ⇄ velnor 1302 lines (1289 timestamped, 89 groups, ansi: true)

## Cache sccache (GitHub) ⇄ Cache sccache (Velnor)

Jobs: github `88058728859` (success) ⇄ velnor `88058728851` (success)

| # | step (github) | step (velnor) | gh concl | vl concl | gh expand | vl expand | gh dur | vl dur | verdict |
|---|---------------|---------------|----------|----------|-----------|-----------|--------|--------|---------|
| 1/1 | Set up job | Set up job | success | success | yes | yes | 1s | 0s | ok |
| 2/2 | Run actions/checkout@v6 | Run actions/checkout@v6 | success | success | yes | yes | 1s | 0s | ok |
| 3/3 | Run mozilla-actions/sccache-action@1583d6b38d7be47f593cb472781bbb21cab4321e | Run mozilla-actions/sccache-action@1583d6b38d7be47f593cb472781bbb21cab4321e | success | success | yes | yes | 0s | 0s | ok |
| 4/4 | Run jdx/mise-action@v4 | Run jdx/mise-action@v4 | success | success | yes | yes | 17s | 0s | ok |
| 5/5 | Run test "${RUSTC_WRAPPER:-}" = sccache | Run test "${RUSTC_WRAPPER:-}" = sccache | success | success | yes | yes | 0s | 0s | ok |
| 6/6 | Run cargo check -p app-a --locked | Run cargo check -p app-a --locked | success | success | yes | yes | 1s | 1s | ok |
| 11/11 | Post Run mozilla-actions/sccache-action@1583d6b38d7be47f593cb472781bbb21cab4321e | Post Run mozilla-actions/sccache-action@1583d6b38d7be47f593cb472781bbb21cab4321e | success | success | yes | yes | 0s | 0s | ok |
| 12/12 | Post Run actions/checkout@v6 | Post Run actions/checkout@v6 | success | success | yes | yes | 0s | 0s | ok |
| 13/13 | Complete job | Complete job | success | success | yes | yes | 0s | 0s | ok |

Lane log content: github 396 lines (395 timestamped, 27 groups, ansi: true) ⇄ velnor 1302 lines (1289 timestamped, 89 groups, ansi: true)

## compat (app-a, GitHub, ubuntu-26.04, false) ⇄ compat (app-a, Velnor, self-hosted, velnor-target-mvp, true)

Jobs: github `88058742143` (success) ⇄ velnor `88058742142` (success)

| # | step (github) | step (velnor) | gh concl | vl concl | gh expand | vl expand | gh dur | vl dur | verdict |
|---|---------------|---------------|----------|----------|-----------|-----------|--------|--------|---------|
| 1/1 | Set up job | Set up job | success | success | yes | yes | 1s | 0s | ok |
| 2/2 | Run actions/checkout@v6 | Run actions/checkout@v6 | success | success | yes | yes | 1s | 0s | ok |
| 3/3 | Run echo "Skipping app-a (packages filter: )" | Run echo "Skipping app-a (packages filter: )" | skipped | skipped | no | no | 0s | 0s | ok |
| 4/4 | Run mozilla-actions/sccache-action@1583d6b38d7be47f593cb472781bbb21cab4321e | Run mozilla-actions/sccache-action@1583d6b38d7be47f593cb472781bbb21cab4321e | success | success | yes | yes | 0s | 0s | ok |
| 5/5 | Run rui314/setup-mold@v1 | Run rui314/setup-mold@v1 | success | success | yes | yes | 1s | 0s | ok |
| 6/6 | Run jdx/mise-action@v4 | Run jdx/mise-action@v4 | success | success | yes | yes | 14s | 0s | ok |
| 7/7 | Run rustup component add rustfmt clippy | Run rustup component add rustfmt clippy | success | success | yes | yes | 1s | 1s | ok |
| 8/8 | Run actions/cache@55cc8345863c7cc4c66a329aec7e433d2d1c52a9 | Run actions/cache@55cc8345863c7cc4c66a329aec7e433d2d1c52a9 | success | success | yes | yes | 0s | 0s | ok |
| 9/9 | Run set -euo pipefail | Run set -euo pipefail | success | success | yes | yes | 0s | 0s | ok |
| 10/10 | Run set -euo pipefail | Run set -euo pipefail | success | success | yes | yes | 0s | 0s | ok |
| 11/11 | Run just fmt-check | Run just fmt-check | success | success | yes | yes | 0s | 0s | ok |
| 12/12 | Run just clippy "app-a" | Run just clippy "app-a" | success | success | yes | yes | 1s | 1s | ok |
| 13/13 | Run just test "app-a" | Run just test "app-a" | success | success | yes | yes | 0s | 0s | ok |
| 14/14 | Run just nextest "app-a" | Run just nextest "app-a" | success | success | yes | yes | 0s | 0s | ok |
| 15/15 | Check MSRV | Check MSRV | success | success | yes | yes | 1s | 9s | ok |
| 16/16 | Run ./.github/actions/check-fixture-output | Run ./.github/actions/check-fixture-output | success | success | yes | yes | 0s | 0s | ok |
| 17/17 | Run python3 .github/scripts/write-result.py "app-a" "GitHub" "out/result.json" | Run python3 .github/scripts/write-result.py "app-a" "Velnor" "out/result.json" | success | success | yes | yes | 0s | 0s | ok |
| 18/18 | Run actions/upload-artifact@v7 | Run actions/upload-artifact@v7 | success | success | yes | yes | 0s | 1s | ok |
| 34/34 | Post Run actions/cache@55cc8345863c7cc4c66a329aec7e433d2d1c52a9 | Post Run actions/cache@55cc8345863c7cc4c66a329aec7e433d2d1c52a9 | success | success | yes | yes | 0s | 0s | ok |
| 35/35 | Post Run mozilla-actions/sccache-action@1583d6b38d7be47f593cb472781bbb21cab4321e | Post Run mozilla-actions/sccache-action@1583d6b38d7be47f593cb472781bbb21cab4321e | success | success | yes | yes | 1s | 0s | ok |
| 36/36 | Post Run actions/checkout@v6 | Post Run actions/checkout@v6 | success | success | yes | yes | 0s | 0s | ok |
| 37/37 | Complete job | Complete job | success | success | yes | yes | 0s | 0s | ok |

Lane log content: github 754 lines (751 timestamped, 40 groups, ansi: true) ⇄ velnor 1302 lines (1289 timestamped, 89 groups, ansi: true)

## compat (app-b, GitHub, ubuntu-26.04, false) ⇄ compat (app-b, Velnor, self-hosted, velnor-target-mvp, true)

Jobs: github `88058742164` (success) ⇄ velnor `88058742156` (success)

| # | step (github) | step (velnor) | gh concl | vl concl | gh expand | vl expand | gh dur | vl dur | verdict |
|---|---------------|---------------|----------|----------|-----------|-----------|--------|--------|---------|
| 1/1 | Set up job | Set up job | success | success | yes | yes | 2s | 0s | ok |
| 2/2 | Run actions/checkout@v6 | Run actions/checkout@v6 | success | success | yes | yes | 0s | 0s | ok |
| 3/3 | Run echo "Skipping app-b (packages filter: )" | Run echo "Skipping app-b (packages filter: )" | skipped | skipped | no | no | 0s | 0s | ok |
| 4/4 | Run mozilla-actions/sccache-action@1583d6b38d7be47f593cb472781bbb21cab4321e | Run mozilla-actions/sccache-action@1583d6b38d7be47f593cb472781bbb21cab4321e | success | success | yes | yes | 1s | 0s | ok |
| 5/5 | Run rui314/setup-mold@v1 | Run rui314/setup-mold@v1 | success | success | yes | yes | 0s | 0s | ok |
| 6/6 | Run jdx/mise-action@v4 | Run jdx/mise-action@v4 | success | success | yes | yes | 12s | 0s | ok |
| 7/7 | Run rustup component add rustfmt clippy | Run rustup component add rustfmt clippy | success | success | yes | yes | 1s | 1s | ok |
| 8/8 | Run actions/cache@55cc8345863c7cc4c66a329aec7e433d2d1c52a9 | Run actions/cache@55cc8345863c7cc4c66a329aec7e433d2d1c52a9 | success | success | yes | yes | 0s | 0s | ok |
| 9/9 | Run set -euo pipefail | Run set -euo pipefail | success | success | yes | yes | 0s | 0s | ok |
| 10/10 | Run set -euo pipefail | Run set -euo pipefail | success | success | yes | yes | 0s | 0s | ok |
| 11/11 | Run just fmt-check | Run just fmt-check | success | success | yes | yes | 0s | 0s | ok |
| 12/12 | Run just clippy "app-b" | Run just clippy "app-b" | success | success | yes | yes | 1s | 0s | ok |
| 13/13 | Run just test "app-b" | Run just test "app-b" | success | success | yes | yes | 0s | 0s | ok |
| 14/14 | Run just nextest "app-b" | Run just nextest "app-b" | success | success | yes | yes | 0s | 1s | ok |
| 15/15 | Check MSRV | Check MSRV | success | success | yes | yes | 0s | 8s | ok |
| 16/16 | Run ./.github/actions/check-fixture-output | Run ./.github/actions/check-fixture-output | success | success | yes | yes | 0s | 1s | ok |
| 17/17 | Run python3 .github/scripts/write-result.py "app-b" "GitHub" "out/result.json" | Run python3 .github/scripts/write-result.py "app-b" "Velnor" "out/result.json" | success | success | yes | yes | 0s | 0s | ok |
| 18/18 | Run actions/upload-artifact@v7 | Run actions/upload-artifact@v7 | success | success | yes | yes | 1s | 1s | ok |
| 34/34 | Post Run actions/cache@55cc8345863c7cc4c66a329aec7e433d2d1c52a9 | Post Run actions/cache@55cc8345863c7cc4c66a329aec7e433d2d1c52a9 | success | success | yes | yes | 0s | 0s | ok |
| 35/35 | Post Run mozilla-actions/sccache-action@1583d6b38d7be47f593cb472781bbb21cab4321e | Post Run mozilla-actions/sccache-action@1583d6b38d7be47f593cb472781bbb21cab4321e | success | success | yes | yes | 0s | 0s | ok |
| 36/36 | Post Run actions/checkout@v6 | Post Run actions/checkout@v6 | success | success | yes | yes | 1s | 0s | ok |
| 37/37 | Complete job | Complete job | success | success | yes | yes | 0s | 0s | ok |

Lane log content: github 754 lines (751 timestamped, 40 groups, ansi: true) ⇄ velnor 1302 lines (1289 timestamped, 89 groups, ansi: true)

## Postgres service (GitHub) ⇄ Postgres service (Velnor)

Jobs: github `88058728876` (success) ⇄ velnor `88058728873` (success)

| # | step (github) | step (velnor) | gh concl | vl concl | gh expand | vl expand | gh dur | vl dur | verdict |
|---|---------------|---------------|----------|----------|-----------|-----------|--------|--------|---------|
| 1/1 | Set up job | Set up job | success | success | yes | yes | 0s | 0s | ok |
| 2/2 | Initialize containers | Initialize containers | success | success | yes | yes | 22s | 15s | ok |
| 3/3 | Run apt-get update && apt-get install -y --no-install-recommends postgresql-client | Run apt-get update && apt-get install -y --no-install-recommends postgresql-client | success | success | yes | yes | 8s | 6s | ok |
| 4/4 | Run test "$(psql -h postgres -U postgres -d fixture -Atc 'SELECT 41 + 1')" = 42 | Run test "$(psql -h postgres -U postgres -d fixture -Atc 'SELECT 41 + 1')" = 42 | success | success | yes | yes | 0s | 0s | ok |
| 8/5 | Stop containers | Stop containers | success | success | yes | yes | 1s | 1s | ok |
| 9/6 | Complete job | Complete job | success | success | yes | yes | 0s | 0s | ok |

Lane log content: github 407 lines (406 timestamped, 12 groups, ansi: true) ⇄ velnor 1302 lines (1289 timestamped, 89 groups, ansi: true)

## Result

**PASS** — no paired step is less informative than the GitHub lane.

Known documented divergence (not gated): V2 jobs have no v1 log archive, so the per-job raw-log download 404s on the Velnor lane; the `job-log` artifact is the workaround (master-plan P4.3).
