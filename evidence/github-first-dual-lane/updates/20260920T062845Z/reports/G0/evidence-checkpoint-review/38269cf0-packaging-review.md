# Independent packaging-only review: `38269cf0ebe2c6c007f80fa78458fc63d40f76a5`

Review date: 2026-09-20. Read-only. No source checkout, runtime, Docker, host, install, build, dispatch, release, publication, or repository mutation was performed.

## Verdict

Packaging integrity: **PASS**.

This is a normal append-only historical evidence update: 55 added files, all under one resolved update path, with 51 selected source files and four package-control files. It is **not** a G0--G7 approval, runtime result, release attestation, current-source assertion, or v10 authority approval. The excluded v10 adversarial side tree remains excluded and cannot be read as a v10 pass.

## Candidate and append-only proof

- Candidate: `38269cf0ebe2c6c007f80fa78458fc63d40f76a5`; parent: `d25819f5b4a9f1e7724456693e9d8061182cdd1a`.
- Remote `origin` is `git@github.com:tailrocks/velnor.git`; `refs/heads/evidence/github-first-dual-lane-20260919T181508Z` resolves to the candidate exactly.
- The actual update path resolved from the parent-to-candidate diff is exactly `evidence/github-first-dual-lane/updates/20260920T052649Z/`.
- Parent-to-candidate diff: 55 paths, all `A`, all under that path; no deletion, modification, or path escape. `git diff --check` passed.
- Git tree: 55/55 regular `100644` blobs; symlinks: 0.
- Cutoff is `2026-09-20T05:32:10Z`; parent/cutoff/source-base bindings are recorded at `checkpoint.md:3-8` and `checkpoint.json:97-115`.

## Inventory and manifest integrity

- The checkpoint declares 51 selected regular files, 1,018,389 source bytes, G0=11/G1=40, and 27 selected JSON reports/fixtures (`checkpoint.md:7`, `checkpoint.json:15-20,74`). The four package-control files are `INVENTORY.tsv`, `SHA256SUMS`, `checkpoint.json`, and `checkpoint.md`; therefore the tree count is 51+4=55.
- `INVENTORY.tsv` has 51 data rows, 1,018,389 summed bytes, and no malformed rows. Independent reread of every recorded external source path found no missing file, size mismatch, source-hash mismatch, or copied-destination hash mismatch.
- `SHA256SUMS` has 54 entries: every tree file except the manifest itself. `sha256sum --check --strict` passed all 54 entries. This covers the package controls and all 51 copied source files.
- All 28 JSON files physically present in the package parse successfully. The count reconciles: 27 selected JSON reports/fixtures plus packaging `checkpoint.json`.
- Secret-pattern scan: 0 matches. No symlinks were selected or dereferenced. These policies and reconciliation claims are also recorded at `checkpoint.json:75-84` and `checkpoint.md:14-16`.

## Frozen v10 closure

- Bundle index records schema v10, root digest `44f144bee0d2437fedf3d72c951d5df248033f7b78100004d81e30980e046136`, root-manifest raw SHA `9f9306f71a82f656affa23eb8957d844ebbbe3be5d3fce293cf9f5bbe0dfac3e`, and `v9_preserved=true` (`reports/G1/bootstrap-transition/v10-contract-bundle-2026-09-20/bundle-index.json:1-10`).
- Independent recomputation obtained the same raw root-manifest SHA and canonical root digest. All 18 root-manifest bound files were present and SHA-matched; no bound-file mismatch.
- The bundled audit is explicitly design-only: 36/36 structural checks passed, `authority_claim=false`, and external provider/freeze/live/native blockers remain unresolved (`reports/G1/bootstrap-transition/v10-contract-bundle-2026-09-20/v10-independent-audit-report.md:1-5,28-48`). Synthetic fixtures are explicitly non-live (`reports/G1/bootstrap-transition/v10-contract-bundle-2026-09-20/canonical-root-manifest.v3.json:102-108`).
- `checkpoint.json:5-11` explicitly excludes the v10 adversarial-audit side tree while retaining only the frozen hash-closed bundle. That exclusion is a scope boundary, not approval evidence.

## Mode-finding chronology

The original fba review, exact Homebrew cross-check, and 05:31Z mode-adjudication addendum are all retained in the update (`checkpoint.md:10-12`; `checkpoint.json:26-30,148-169`). Their recorded SHA-256 values are independently covered by the package manifest (`checkpoint.json:94-95`). The addendum supersedes only the historical executable-mode residual; it does not approve the overall fba handoff. Synthetic Mach-O, APT parent binding, immutable preview, and runtime/publication blockers remain explicitly open.

## Boundary

The package records `attestation: none` and `gate_status: not-evaluated`; its gate statement establishes no G0, G1, G2, G3, G4, G5, G6, or G7 success (`checkpoint.json:3-14`). Reports are immutable historical evidence only. No source, GitHub authority, ruleset, check, release, dispatch, merge, install, host, or credential state was changed (`checkpoint.json:106-113`).

Final disposition: **PASS for packaging integrity only; no gate or v10 approval.**
