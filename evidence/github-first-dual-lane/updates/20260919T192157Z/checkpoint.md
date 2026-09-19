# External evidence checkpoint update

Update: `20260919T192157Z`  \
Captured: `2026-09-19T19:24:28Z` UTC  \
Evidence ref: `refs/heads/evidence/github-first-dual-lane-20260919T181508Z`  \
Parent checkpoint: `a1f9f1187a6cc6cb2197836ca03e79f1c5261a09`  \
Source base context: `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`

This append-only snapshot adds stable compact review reports, typed JSON/TSV
records, synthetic hostile fixtures, the latest estate-scope review, and the
coordinator's final push census. It does not attest source, establish a gate,
or claim G0-G7 success. The parent ref was fetched first and matched local
`a1f9f118...` before copying.

## Included

71 pre-manifest files under `reports/`:

- `G0/checker-adversarial-fixtures/`: 54 synthetic fixture JSON/manifest files,
  generator/run scripts, report, and compact `results.ndjson`. The fixture
  report reviews checker `2ba66b116dd5511f0b4f2a6856cfbed6bd290152` and records
  positive controls plus hostile cases; it does not approve the checker.
- `G0/checker-identity/`: owner/tree reconciliation for checker
  `2ba66b116dd5511f0b4f2a6856cfbed6bd290152`.
- `G0/checker-host-release-adversarial.md` and
  `G0/checker-v2-review/`: compact checker host/release and schema fixture
  reviews, design findings only.
- `G0/fleet/`: records completeness review, 32-row workload projection,
  prior coordinator push census (`captured_at=2026-09-19T18:35:57Z`, source
  SHA-256 `8faaa8af6224e45e4ebf7f908be731680bb37061b31d83b6ce8817ac9aad410f`),
  and the latest current push/checkpoint census (`captured_at=2026-09-19T19:23:22Z`,
  SHA-256 `a02ad7c04d268ac69fe0d974395bf12c13bdf05f77e029a10dfe11d75c875e78`).
- `G0/estate-scope-review/`: independent fixed32/auxiliary-boundary report
  and adversarial scope fixtures; exact scope commit `6387532f...` passes its
  bounded tests but retains the documented caller-plan residual.
- `G1/reviews/hosted-provider-3aecc6ed.md`: exact hosted-provider review;
  verdict is do not approve because one release test and producer-consumer
  trust gaps remain.

The records review explicitly says incomplete/blocked: one stale historical
SHA, absent per-row PR/check/workflow/provider/run/workload evidence, and no
G0 pass. Workload data is a source-observed projection, not execution success.
The prior push census reports 18/18 clean/local-remote-equal worktrees. The
latest current census reports 21 selected trees clean and remote-equal,
including two normal-forward attribution corrections; it remains external
evidence only. `dual-lane-apt-schema2` is recorded as mixed WIP with compile
errors, so this is not a gate result.

## Validation

- 61 JSON/`.manifest` files parsed with `jq -e .`.
- 34/34 `results.ndjson` lines parsed as JSON.
- Secret-pattern scan over all included files: zero hits.
- `INVENTORY.tsv` and `SHA256SUMS` cover the copied files; the SHA manifest
  excludes itself.

Referenced attribution/source commits are observations only: checker
`d9a277938d54d93f06b85ce6e8d406ebb67468c3`, APT schema `24438704e22b9a82b3240cd06e2aae0eb2fe1d95`, records `92f6ff62...`, and estate
scope `6387532f...`. Historical trailer concerns (`2ba`/`c63`) remain recorded
as context, not silently treated as repaired gate evidence.

## Excluded

Excluded from this update: generated checker `out/*.stdout`/`out/*.stderr`
logs, research/cloned repositories, build targets/binaries, caches, raw
mass logs, credentials, mutable session state, and ongoing writes. Earlier
checkpoint files remain preserved in prior update directories; this update
does not rewrite them. No source checkout was changed.

`G0/lane-compare-review/review-3ad452acd.md` was observed at
`2026-09-19T19:27:17Z`, after the `19:24:28Z` cutoff, so it is explicitly
excluded from this checkpoint and must be captured in a later append-only
update.

`attestation=none`; `gate_status=not-evaluated`.
