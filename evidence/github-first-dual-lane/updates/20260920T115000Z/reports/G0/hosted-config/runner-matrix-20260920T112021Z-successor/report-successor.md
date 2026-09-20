# Hosted runner matrix correction successor

Successor captured UTC: `2026-09-20T11:20:21Z`.

Predecessor remains untouched at `../runner-matrix-20260920T110534Z`. Its matrix SHA256 is `84c5f34980c62cf874f3135b0cf01ff388d1dada923fad464583e365d11f723c`; the preserved copy is `predecessor-matrix.json`.

## Correction

Independent review `23ec799d7577e92992b0aff003ffafa4567325c4caea0fd0da0512310caef324` found the predecessor collapsed ordinary Windows Server 2025 and Visual Studio 2026 labels into one doc/version. This successor re-derives all 29 label mappings from the pinned root table plus each image README.

The corrected mapping is:

| Label | Architecture | Image README | Image version | Git blob SHA | Decoded-content SHA256 | Preview |
|---|---|---|---|---|---|---|
| `windows-latest` | x64 | `images/windows/Windows2025-Readme.md` | `20260913.261.1` | `355fd966442e6144a452d39f9ac18e8ac2081156` | `9c18074f7a9d99a636dfd3b440a92ca8b36f83a097a1e7b5168b6860db36ee80` | false |
| `windows-2025` | x64 | `images/windows/Windows2025-Readme.md` | `20260913.261.1` | `355fd966442e6144a452d39f9ac18e8ac2081156` | `9c18074f7a9d99a636dfd3b440a92ca8b36f83a097a1e7b5168b6860db36ee80` | false |
| `windows-2025-vs2026` | x64 | `images/windows/Windows2025-VS2026-Readme.md` | `20260907.229.1` | `f1b4942a764bc63f489c8ddfcd3ed43ed5baebf8` | `eb5c9f5f43b41def440059077ef5354c5732c5a5f682c963105d4904e6752c1f` | false |

The authoritative full mapping is `matrix.json` / `matrix-corrected.json`. `rederivation-checks.txt` has `ROOT_OK` for every label and `DOC_OK` for every mapped image README; only `ubuntu-slim` is explicitly `DOC_UNKNOWN` because the pinned tree has no dedicated image README, so its image version remains unknown.

## Re-derived matrix facts

- Source commit: `actions/runner-images` `main` at `9d7eec51794bb688d6c7ce00550cda5a134c88a3`; root README Git blob `8de90fd8b3b5bf9aeec88151e87a3469973bbecd`.
- 29 labels map to explicit architecture, README path, image version (or explicit unknown), Git blob SHA, and decoded-content SHA256.
- Explicit public-preview labels only: `ubuntu-26.04`, `ubuntu-26.04-arm`, `xcode-27`, `xcode-27-xlarge`.
- `macos-27`, `macos-27-intel`, and `windows-11-x64` remain absent from the pinned root table. macOS 27 is exposed only through arm64 `xcode-27` preview labels.
- No entitlement, account access, capacity, or runtime routing inference was made. No architecture fallback was introduced.

## Evidence integrity

- `source-label-table.txt` preserves the root label/architecture table.
- `preview-flag-evidence.txt` preserves explicit preview, deprecation, GA, and transition passages.
- `raw/` is the predecessor's pinned HTTP/body source capture, unchanged; all source content hashes are retained.
- `predecessor-*.json|md` preserve the prior successor inputs for comparison; the predecessor directory itself remains unchanged.
- New `manifest.sha256` covers the successor directory. No workflow/source files, dispatches, checks, logs, merges, or cancellations were touched.
