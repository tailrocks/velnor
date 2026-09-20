# Hosted runner matrix correction successor: bounded review

Date: 2026-09-20

Scope: read-only review of `G0/hosted-config/runner-matrix-20260920T112021Z-successor`. No runtime, entitlement, dispatch, or workflow claim.

## Identity and integrity

- `matrix.json` and `matrix-corrected.json` both SHA-256: `381ed3717b14e9e74943d1aa02fa314a9cc2fa858e6c5656e53ed0075332b4ce`.
- `manifest.sha256` SHA-256: `eaeaf361842c3317218193f06888f082be7c939e02f8392bd3cbfafd913c324a`.
- Manifest verification: `138/138` entries `OK`; zero missing/extra paths.
- Predecessor matrix independently remains `84c5f34980c62cf874f3135b0cf01ff388d1dada923fad464583e365d11f723c` (original artifact manifest still `129/129` OK). Successor `predecessor-matrix.json` has the same SHA.
- Source remains `actions/runner-images` `main` commit `9d7eec51794bb688d6c7ce00550cda5a134c88a3`; root README blob `8de90fd8b3b5bf9aeec88151e87a3469973bbecd`. Ref/tree resolve to that commit, tree untruncated, and indexed paths match tree blobs.

## Mapping checks

- `29/29` labels are unique and all occur in the pinned root README's `18` source rows; architecture pairs match.
- `28/28` mapped image documents match API metadata, Git blob SHA, decoded-content SHA-256, byte length, and image-version text. `ubuntu-slim` is the sole explicit `DOC_UNKNOWN`; it has no dedicated README in the pinned tree and retains `image_version: null`.
- Preview flags are exact: only `ubuntu-26.04`, `ubuntu-26.04-arm`, `xcode-27`, and `xcode-27-xlarge` are preview labels.
- `macos-27`, `macos-27-intel`, and `windows-11-x64` remain absent. Xcode 27 remains arm64-only; no architecture fallback is introduced.
- The corrected `windows-2025-vs2026` mapping is exact: x64, `images/windows/Windows2025-VS2026-Readme.md`, version `20260907.229.1`, Git blob `f1b4942a764bc63f489c8ddfcd3ed43ed5baebf8`, decoded SHA-256 `eb5c9f5f43b41def440059077ef5354c5732c5a5f682c963105d4904e6752c1f`. No stale mapping to ordinary `Windows2025-Readme.md` remains.

## Boundary

The source proves public labels and image/document identity only. `actions/runner` `v2.337.0` remains runner software, not an image version. Account entitlement, larger-runner access, capacity, routing, and actual hosted execution remain unknown; no entitlement or runtime inference is allowed.

## Verdict

Successor correction closes the predecessor's Windows 2025/VS2026 crossmapping defect. Source and mapping checks are clean within scope. `ubuntu-slim` is correctly fail-closed as version-unknown. This remains a point-in-time public-source capability reference, not an execution gate.
