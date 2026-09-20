# Real GitHub API check-suite supplement — corrected metadata successor

This directory is an append-only metadata successor for:

- original directory: `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G0/real-api-fixture-supplement-20260920T085311Z`
- original manifest SHA-256: `1146764830a0e6e31518dd05abd6c00e6627f554d6dbbc97a55d721facabe70a`
- original relationship report SHA-256: `c1ab3d2eb6802be4e9c67ba07b3ef76e4deeb88757ac31b20a6689224bc69e12`
- original README SHA-256: `8fd79fe81ebd56ccf809a3546558ca7c66d1c868573ccc199b266f07773bcaae`

The original manifest, relationship report, README, and all 72 original raw bytes remain unchanged. This successor stores metadata only and points at the frozen original raw source through `raw_source_root`; it does not duplicate or rewrite raw files.

## Correction

Request `velnor-check-suite-96108227551` is corrected from the original malformed metadata to:

- `response_date_header`: `Sun, 20 Sep 2026 08:53:22 GMT`
- `link_header`: `absent`

Both values are read directly from `raw/check-suites/velnor/suite-96108227551.http`. The same direct derivation was applied to all 18 request records in both successor JSON files.

## Derivation and regression

Metadata is parsed line-by-line from each referenced raw `.http` header file. Date and Link values are independent fields; missing fields become the literal `absent`. Only the line-ending CR and leading field whitespace are removed; spaces and tabs inside field values are retained. `has_next` is derived from the exact Link relation token. No TSV parsing is used.

The generation parser passed regressions for independent missing Date/Link fields, dates containing spaces, leading-tab field whitespace, Link values containing tabs, and structured-object emission without TSV column shifting.

## Scope

This is read-only source evidence for checker/mapper regression. It makes no execution, live-gate, authority, rollout, or dispatch claim. The quarantined incomplete capture `real-api-fixture-supplement-20260920T085207Z-incomplete-422` is excluded.

Successor manifest SHA-256: `fea65b61ff1668ff58762066b207ac1c9534452d328080f1f0680e7cb571447e`
Successor relationship-report SHA-256: `dbcc5a2d331463c12253c49203ac54ac7d8d0df1824333e6cc90c61f9118893d`
README is frozen after this text is written.
