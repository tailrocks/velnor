# Hosted runner matrix: bounded independent review

Date: 2026-09-20

Scope: read-only review of `G0/hosted-config/runner-matrix-20260920T110534Z`. This verifies public-source provenance and matrix semantics only. It does not prove Velnor account entitlement, scheduling, capacity, billing, or a successful hosted job.

## Provenance and integrity

- `actions/runner-images` exact `main` source: `9d7eec51794bb688d6c7ce00550cda5a134c88a3`.
- `actions/runner` exact `main` source: `80bb1fb827fa44d489263061e71ef4adba7ad8cd`.
- Latest captured runner release: `v2.337.0`, tag commit `397b032cbf865e9c3ddfab89d533ec19325e1273`, published `2026-08-26T14:33:29Z`.
- `shasum -a 256 -c manifest.sha256`: all `129/129` listed artifacts `OK`; the directory contains exactly those 129 artifacts plus `manifest.sha256`.
- All 19 `actions/runner-images` content-index entries (root README plus 18 image docs) independently match API body metadata, Git blob SHA (`blob <length>\0<bytes>`), byte length, decoded bytes, and metadata. All 19 decoded-content SHA-256 entries match.
- The preserved runner README body similarly matches its index SHA, Git blob SHA, and length. All captured API responses are HTTP 200; no API errors, credentials, dispatches, checks, or logs are recorded.
- The runner-images ref and tree both resolve to `9d7eec...`; tree is untruncated (588 entries), and every indexed content path exists as the exact tree blob (`19/19`).

## Matrix verification

The matrix has 18 rows and 29 unique labels: 9 x64 rows and 9 arm64 rows. Independent parsing of the pinned root README yields the same 18 rows and 29 labels with no missing or extra label/architecture pair.

- Ubuntu 26.04: x64 `ubuntu-26.04` and arm64 `ubuntu-26.04-arm`; both public preview.
- Ubuntu 24.04: x64 `ubuntu-latest`, `ubuntu-24.04`; arm64 `ubuntu-24.04-arm`.
- Ubuntu 22.04: x64/arm64 labels present and deprecation announced.
- Xcode/macOS 27: arm64 only, `xcode-27` / `xcode-27-xlarge`, public preview. No `macos-27` label is present; the matrix records `macos_27_label_observed=false`.
- macOS 26: x64 `macos-latest-large`, `macos-26-intel`, `macos-26-large`; arm64 `macos-latest`, `macos-26`, `macos-26-xlarge`.
- Windows: Server 2025 x64, Server 2022 x64, Windows 11 arm64, and Windows 11 VS2026 arm64. No Windows 11 x64 label is listed.

## Finding: Windows 2025 label mapping is not exact

The matrix row currently claims one source/version for three labels:

```text
labels:     windows-latest, windows-2025, windows-2025-vs2026
image_doc:  images/windows/Windows2025-Readme.md
version:    20260913.261.1
```

The pinned root README's canonical link for that row is `[windows-2025-vs2026] -> images/windows/Windows2025-VS2026-Readme.md`. The preserved VS2026 document reports image version `20260907.229.1`, distinct from `20260913.261.1`; both documents are present in the content index. Therefore the matrix cannot claim an exact label-to-image/version mapping for `windows-2025-vs2026` as written. Split the label mapping or attach each label to its authoritative document/version before using it as a strict runner capability input. Do not silently treat the three labels as one immutable image.

## Runtime boundaries

`actions/runner-images` image versions (for example `20260907.300.1`) are VM image versions. `actions/runner` `v2.337.0` is runner application software. The evidence proves neither that hosted Velnor jobs currently run that runner version nor that any listed label is enabled for this account.

Large/xlarge and arm64 labels may depend on repository/organization plan entitlement. No entitlement inference is valid from public labels. Unknown/inaccessible labels must remain explicit failures or unresolved capability; no x64/arm64 or OS fallback is authorized.

## Verdict

Public source pins, raw/API/Git provenance, row/label/architecture census, Ubuntu 26 preview, Xcode 27 arm64-only preview, and Windows architecture inventory are independently verified. The Windows 2025/VS2026 source-version conflation is a remaining contract defect. This artifact is a useful point-in-time capability reference, not hosted-runtime proof or an execution gate.
