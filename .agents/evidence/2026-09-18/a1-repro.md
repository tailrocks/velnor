# A1 reproduction: velnor Policy generated-tree failure

Date (UTC): 2026-09-16
Workspace: /Users/donbeave/Projects/tailrocks/velnor-project/velnor3
HEAD: 38ffbfd7 docs(bastion): fix markdownlint errors in plan files
Tree: clean (`git status --short` empty) before and after; no files edited, no rebuild needed, no commit.

## Binary freshness

`target/debug/velnor-workflow` built Sep 17 06:40 (local), no source file newer
(`find crates/velnor-workflow/src Cargo.toml crates/velnor-workflow/Cargo.toml -newer target/debug/velnor-workflow` → empty).
No rebuild performed.

- `--revision`: 38ffbfd73bc67002ddce7e38fdaf15ff6497317d (matches HEAD)
- `--closure`: 98e0ebd598ba6d148508aee0b6945a317ba00fac28d51411f1bc519d24fe14ef

## Command 1: ./target/debug/velnor-workflow --plain --dry-run

Exit: 0

Generated-files section (verbatim):

```
  = unchanged .github/actionlint.yaml                    actionlint runner-label contract
  = unchanged .github/actions/report-velnor-ci-outcomes/action.yml generated CI support file
  = unchanged .github/actions/setup-velnor-workflow/action.yml generated CI support file
  = unchanged .github/ci/project.toml                    detected CI graph + binary runtime contract
  = unchanged .github/workflows/AGENTS.md                workflow directory rule file
  = unchanged .github/workflows/ci-main.yml              trusted main verification
  = unchanged .github/workflows/ci-policy.yml            base-owned pull-request policy gate
  = unchanged .github/workflows/ci-pr.yml                parallel PR verification
  = unchanged .github/workflows/ci-release-package-signer.yml release artifact provenance signer
  = unchanged .github/workflows/ci-runtime-products.yml  generated CI support file
  = unchanged .github/workflows/ci-unit-bun.yml          generated CI support file
  = unchanged .github/workflows/ci-unit-docker.yml       generated CI support file
  = unchanged .github/workflows/ci-unit-docs.yml         generated CI support file
  = unchanged .github/workflows/ci-unit-opentofu.yml     generated CI support file
  = unchanged .github/workflows/ci-unit-rust.yml         generated CI support file
  = unchanged .github/workflows/maintenance.yml          closed-PR cache cleanup
  = unchanged .github/workflows/nightly.yml              scheduled full verification
  = unchanged .github/workflows/preview.yml              preview artifact workflow
  = unchanged .github/workflows/release.yml              release publisher workflow
  = unchanged config/fleet/velnor-host.env               generated CI support file
  = unchanged .github/ci/.github-actions-generator-state ownership and overwrite safety state
```

Result line:

```
Result Dry-run: 0 files would change in /Users/donbeave/Projects/tailrocks/velnor-project/velnor3
```

Local differing-file list: (empty — 0 files)

## Comparison with run 35129353335

Run 35129353335 reported 3 differing files:

1. .github/ci/.github-actions-generator-state
2. ci-main.yml
3. ci-policy.yml

Local dry-run: all three report `= unchanged`. Zero overlap with the CI failure set.

## Extra evidence: ./target/debug/velnor-workflow --plain --check

Exit: 0

```
Result Generated files are current in /Users/donbeave/Projects/tailrocks/velnor-project/velnor3
```

Same verdict as dry-run: tree is current, no writes, no failure.

## Command 2: cargo test -p velnor-workflow --no-run

Exit: 0. Tests compile. Binaries produced:

- target/debug/deps/velnor_workflow-1d366257f802c6b0 (lib unittests)
- target/debug/deps/velnor_workflow-baf5a583a21e2c7f (main unittests)
- target/debug/deps/generic_surface_literals-6d40df010fff45cb
- target/debug/deps/lane_pairing-4bb2db83351733a9
- target/debug/deps/selection_artifact_handoff-ece90de29279fc01
- target/debug/deps/synthetic_surface-b0ee8db3681d125d
- target/debug/deps/velnor_first_ci-cafe32b99acee150

No test was executed (compile only, per task).

## Verdict

NOT REPRODUCED.

At HEAD 38ffbfd7 with a fresh binary built from that commit, both `--plain --dry-run`
(exit 0, 0 files would change) and `--plain --check` (exit 0, files current) pass
locally. The 3-file diff from run 35129353335
(.github/ci/.github-actions-generator-state, ci-main.yml, ci-policy.yml) does not appear.
Divergence therefore comes from outside the local tree state — e.g. CI running at a
different commit, different flags/inputs, or an environment-dependent render — not from
a locally observable stale generated tree.
