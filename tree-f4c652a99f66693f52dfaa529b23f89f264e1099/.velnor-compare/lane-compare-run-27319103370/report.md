# Lane compare — run 27319103370 (donbeave/velnor-actions-fixture)

Gate: equal-or-better — zero rows where the GitHub lane shows information the Velnor lane lacks.

## compat (app-a, github, "ubuntu-latest") ⇄ compat (app-a, velnor, ["self-hosted","velnor-target-mvp"])

Jobs: github `80706115084` (success) ⇄ velnor `80706115091` (success)

| # | step (github) | step (velnor) | gh concl | vl concl | gh expand | vl expand | gh dur | vl dur | verdict |
|---|---------------|---------------|----------|----------|-----------|-----------|--------|--------|---------|
| 1 | Set up job | Set up job | success | success | yes | yes | 3s | 0s | ok |
| 2 | Run actions/checkout@v6 | — | success | — | yes | — | 0s | — | WORSE (missing on velnor) |
| 3 | Run echo "Skipping app-a (packages filter: )" | Run echo "Skipping app-a (packages filter: ${{ inputs.packages }})" | skipped | skipped | no | no | 0s | 0s | WORSE (name 'Run echo "Skipping app-a (packages filter: )"' vs 'Run echo "Skipping app-a (packages filter: ${{ inputs.packages }})"') |
| 4 | Run mozilla-actions/sccache-action@v0.0.10 | Run mozilla-actions/sccache-action@v0.0.10 | success | success | yes | yes | 1s | 0s | ok |
| 5 | Run rui314/setup-mold@v1 | Run rui314/setup-mold@v1 | success | success | yes | yes | 1s | 1s | ok |
| 6 | Run jdx/mise-action@v4 | Run jdx/mise-action@v4 | success | success | yes | yes | 1s | 0s | ok |
| 7 | Run rustup component add rustfmt clippy | Run rustup component add rustfmt clippy | success | success | yes | yes | 9s | 0s | ok |
| 8 | Run Swatinem/rust-cache@v2 | Run Swatinem/rust-cache@v2 | success | success | yes | yes | 1s | 0s | ok |
| 9 | Run set -euo pipefail | msrv | success | success | yes | yes | 0s | 0s | WORSE (name 'Run set -euo pipefail' vs 'msrv') |
| 10 | Run set -euo pipefail | command-files | success | success | yes | yes | 0s | 1s | WORSE (name 'Run set -euo pipefail' vs 'command-files') |
| 11 | Run just fmt-check | Run just fmt-check | success | success | yes | yes | 0s | 0s | ok |
| 12 | Run just clippy "app-a" | Run just clippy "app-a" | success | success | yes | yes | 1s | 0s | ok |
| 13 | Run just test "app-a" | Run just test "app-a" | success | success | yes | yes | 0s | 0s | ok |
| 14 | Run just nextest "app-a" | Run just nextest "app-a" | success | success | yes | yes | 0s | 0s | ok |
| 15 | Check MSRV | Check MSRV | success | success | yes | yes | 1s | 36s | ok |
| 16 | Run { | Run { | success | success | yes | yes | 0s | 0s | ok |
| 17 | Run ./.github/actions/check-fixture-output | Run set -euo pipefail | success | success | yes | yes | 0s | 0s | WORSE (name 'Run ./.github/actions/check-fixture-output' vs 'Run set -euo pipefail') |
| 18 | Run python3 .github/scripts/write-result.py "app-a" "github" "out/result.json" | Run python3 .github/scripts/write-result.py "app-a" "velnor" "out/result.json" | success | success | yes | yes | 0s | 1s | ok |
| 19 | Run actions/upload-artifact@v7 | Run actions/upload-artifact@v7 | success | success | yes | yes | 1s | 1s | ok |
| 36 | Post Run Swatinem/rust-cache@v2 | Post Run Swatinem/rust-cache@v2 | success | success | yes | yes | 0s | 0s | ok |
| 37 | Post Run mozilla-actions/sccache-action@v0.0.10 | Post Run mozilla-actions/sccache-action@v0.0.10 | success | success | yes | yes | 0s | 0s | ok |
| 38 | Post Run actions/checkout@v6 | Post Run actions/checkout@v6 | success | success | yes | yes | 0s | 0s | ok |
| 39 | Complete job | Complete job | success | success | yes | yes | 0s | 0s | ok |

Lane log content: github 805 lines (804 timestamped, 40 groups, ansi: true) ⇄ velnor 714 lines (0 timestamped, 88 groups, ansi: true)

**WORSE (lane content):** no timestamps

## compat (app-b, github, "ubuntu-latest") ⇄ compat (app-b, velnor, ["self-hosted","velnor-target-mvp"])

Jobs: github `80706115101` (success) ⇄ velnor `80706115089` (success)

| # | step (github) | step (velnor) | gh concl | vl concl | gh expand | vl expand | gh dur | vl dur | verdict |
|---|---------------|---------------|----------|----------|-----------|-----------|--------|--------|---------|
| 1 | Set up job | Set up job | success | success | yes | yes | 3s | 0s | ok |
| 2 | Run actions/checkout@v6 | — | success | — | yes | — | 1s | — | WORSE (missing on velnor) |
| 3 | Run echo "Skipping app-b (packages filter: )" | Run echo "Skipping app-b (packages filter: ${{ inputs.packages }})" | skipped | skipped | no | no | 0s | 0s | WORSE (name 'Run echo "Skipping app-b (packages filter: )"' vs 'Run echo "Skipping app-b (packages filter: ${{ inputs.packages }})"') |
| 4 | Run mozilla-actions/sccache-action@v0.0.10 | Run mozilla-actions/sccache-action@v0.0.10 | success | success | yes | yes | 1s | 0s | ok |
| 5 | Run rui314/setup-mold@v1 | Run rui314/setup-mold@v1 | success | success | yes | yes | 0s | 1s | ok |
| 6 | Run jdx/mise-action@v4 | Run jdx/mise-action@v4 | success | success | yes | yes | 3s | 0s | ok |
| 7 | Run rustup component add rustfmt clippy | Run rustup component add rustfmt clippy | success | success | yes | yes | 9s | 0s | ok |
| 8 | Run Swatinem/rust-cache@v2 | Run Swatinem/rust-cache@v2 | success | success | yes | yes | 2s | 0s | ok |
| 9 | Run set -euo pipefail | msrv | success | success | yes | yes | 0s | 0s | WORSE (name 'Run set -euo pipefail' vs 'msrv') |
| 10 | Run set -euo pipefail | command-files | success | success | yes | yes | 0s | 1s | WORSE (name 'Run set -euo pipefail' vs 'command-files') |
| 11 | Run just fmt-check | Run just fmt-check | success | success | yes | yes | 0s | 0s | ok |
| 12 | Run just clippy "app-b" | Run just clippy "app-b" | success | success | yes | yes | 1s | 0s | ok |
| 13 | Run just test "app-b" | Run just test "app-b" | success | success | yes | yes | 1s | 0s | ok |
| 14 | Run just nextest "app-b" | Run just nextest "app-b" | success | success | yes | yes | 0s | 1s | ok |
| 15 | Check MSRV | Check MSRV | success | success | yes | yes | 0s | 26s | ok |
| 16 | Run { | Run { | success | success | yes | yes | 0s | 0s | ok |
| 17 | Run ./.github/actions/check-fixture-output | Run set -euo pipefail | success | success | yes | yes | 0s | 0s | WORSE (name 'Run ./.github/actions/check-fixture-output' vs 'Run set -euo pipefail') |
| 18 | Run python3 .github/scripts/write-result.py "app-b" "github" "out/result.json" | Run python3 .github/scripts/write-result.py "app-b" "velnor" "out/result.json" | success | success | yes | yes | 0s | 1s | ok |
| 19 | Run actions/upload-artifact@v7 | Run actions/upload-artifact@v7 | success | success | yes | yes | 2s | 1s | ok |
| 36 | Post Run Swatinem/rust-cache@v2 | Post Run Swatinem/rust-cache@v2 | success | success | yes | yes | 0s | 0s | ok |
| 37 | Post Run mozilla-actions/sccache-action@v0.0.10 | Post Run mozilla-actions/sccache-action@v0.0.10 | success | success | yes | yes | 0s | 0s | ok |
| 38 | Post Run actions/checkout@v6 | Post Run actions/checkout@v6 | success | success | yes | yes | 0s | 0s | ok |
| 39 | Complete job | Complete job | success | success | yes | yes | 0s | 0s | ok |

Lane log content: github 806 lines (805 timestamped, 40 groups, ansi: true) ⇄ velnor 714 lines (0 timestamped, 88 groups, ansi: true)

**WORSE (lane content):** no timestamps

## Result

**FAIL** — 12 row(s) where the Velnor lane is less informative than the GitHub lane.

Known documented divergence (not gated): V2 jobs have no v1 log archive, so the per-job raw-log download 404s on the Velnor lane; the `job-log` artifact is the workaround (master-plan P4.3).
