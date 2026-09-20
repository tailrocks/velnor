# Bounded source-contract review: inventory report v3

Review date: 2026-09-20

## Verdict

PASS for the requested v3 source correction, bounded to source-contract and
provenance claims. This is not a live CI result, runner-availability result,
execution result, release result, or gate approval.

## Exact inputs

- Report under review: `report-v3-20260920T073411Z.md`
  SHA-256: `91b89afd01dcd838527b1737bd967957398da932756de097591315565968b5af`.
- Immutable predecessor `report.md` SHA-256:
  `7fedde0a17e126d258eacce323e02ece0b081d77977fe6c9b239e4e6d1876b37`.
- Immutable predecessor `report-v2-20260920T070955Z.md` SHA-256:
  `50b4441a4891267fca10814fff53de38bcfd166dd556cff3cc3fd8b972d77360`.
  Both predecessor hashes match the v3 assertions.
- Detached source snapshots were clean and at the recorded commits:
  Velnor `d20d4d1d17590cca85b501d982cbaad70d42c641`, tracing
  `f234158eb3d5caddda83b19e527ffb7d0f675a23`, schemalane
  `ab49e2dd910ee41e96daea97ad6998f7961e5cfc`, pg-bigdecimal
  `7dc5267d855801dbffb54aa12fc87791aa000a93`, and Parallax
  `54d09bf71181dcfc72d6829fabec3c53f55aacf9`.

## Source-contract checks

- Parallax `.github/ci/project.toml:92-238` defines exactly ten Rust units:
  `rust-checkout`, `rust-inventory`, `rust-notifications`, `rust-orders`,
  `rust-playground-cli`, `rust-playground-proto`,
  `rust-playground-telemetry`, `rust-pricing`, `rust-recommendation`, and
  `rust-storefront`. Each has GitHub and Velnor callers in both workflows;
  the caller blocks are visible in `ci-main.yml:408-992`, with the required
  list at `ci-main.yml:1014-1018`. This supports the corrected “10 paired
  Rust” statement.
- Swift has only the `github-swift-package-macos` caller, with `lane:
  github`, in `ci-main.yml:996-1004` and `ci-pr.yml:889-897`. The child
  `.github/workflows/ci-unit-swift.yml:100-105` declares only
  `verify-github` on `ubuntu-24.04`. No `velnor-swift-package-macos` caller
  or Darwin runner declaration is present. This supports the corrected
  GitHub-only Swift/Ubuntu/no-Darwin statement.
- The raw workflow-source capture is immutable at
  `workflow-sources.json` SHA-256
  `96b2ed5becf5028b40c57217cb0ec193a891566c8f1924cf66f7dd56b6356c97`:
  Velnor has 210 rows across 15 revisions and 14 paths (15 x 14), with no
  `d20d4d1d17590cca85b501d982cbaad70d42c641` row. Parallax has 581 tracked
  entries: 466 blobs and 115 trees. These are source/listing observations,
  not execution or success claims.

## Method and limits

This review compared the v2/v3 reports, recomputed their SHA-256 hashes, and
inspected the detached source snapshots and immutable raw capture metadata.
No source, workflow, report, runner, or product files were changed; no build,
test, dispatch, install, or release operation was performed. The conclusion
is limited to the claims listed above.
