# Official GitHub-hosted runner matrix refresh

Captured UTC: `2026-09-20T11:05:34Z`.

Primary pins:

- `actions/runner-images` `main`: commit `9d7eec51794bb688d6c7ce00550cda5a134c88a3`; root README Git blob `8de90fd8b3b5bf9aeec88151e87a3469973bbecd`.
- `actions/runner` latest release: `v2.337.0`, published `2026-08-26T14:33:29Z`, tag commit `397b032cbf865e9c3ddfab89d533ec19325e1273`; release-tag README Git blob `0279d839ea5015e63a7136ae5116a1b52e66c97c`.

## Current labels

The labels below are copied from the pinned `actions/runner-images` root README. Architecture is image architecture, not an inferred fallback.

| Image | Arch | Labels | State |
|---|---|---|---|
| Ubuntu 26.04 | x64 | `ubuntu-26.04` | public preview |
| Ubuntu 26.04 | arm64 | `ubuntu-26.04-arm` | public preview |
| Ubuntu 24.04 | x64 | `ubuntu-latest`, `ubuntu-24.04` | GA baseline |
| Ubuntu 24.04 | arm64 | `ubuntu-24.04-arm` | GA baseline |
| Ubuntu 22.04 | x64 | `ubuntu-22.04` | deprecation announced |
| Ubuntu 22.04 | arm64 | `ubuntu-22.04-arm` | deprecation announced |
| Ubuntu Slim | x64 | `ubuntu-slim` | special image |
| macOS 27/Xcode image | arm64 | `xcode-27`, `xcode-27-xlarge` | public preview |
| macOS 26 | x64 | `macos-latest-large`, `macos-26-intel`, `macos-26-large` | GA baseline |
| macOS 26 | arm64 | `macos-latest`, `macos-26`, `macos-26-xlarge` | GA baseline |
| macOS 15 | x64 | `macos-15-large`, `macos-15-intel` | GA |
| macOS 15 | arm64 | `macos-15`, `macos-15-xlarge` | GA |
| macOS 14 | x64 | `macos-14-large` | deprecated |
| macOS 14 | arm64 | `macos-14`, `macos-14-xlarge` | deprecated |
| Windows Server 2025 | x64 | `windows-latest`, `windows-2025`, `windows-2025-vs2026` | GA |
| Windows Server 2022 | x64 | `windows-2022` | GA |
| Windows 11 | arm64 | `windows-11-arm` | GA |
| Windows 11 + Visual Studio 2026 | arm64 | `windows-11-vs2026-arm` | GA |

No Windows 11 x64 label is listed. No `macos-27` label is listed. The only macOS 27 source image is the arm64 Xcode preview, exposed through `xcode-27` labels. Do not synthesize `macos-27`, `macos-27-intel`, or an architecture fallback.

The Windows 11 Arm64 docs carry an announcement that `windows-11-arm` will use the Visual Studio 2026 image in September 2026. The static source snapshot does not prove the exact post-transition runtime image; treat that mapping as transition-sensitive until a permitted hosted run confirms it.

## Baseline versus accepted Xcode 27

- Ubuntu baseline: use `ubuntu-24.04` for x64 and `ubuntu-24.04-arm` for arm64. The pinned image docs report image versions `20260907.300.1` and `20260907.118.1` respectively.
- macOS baseline: use `macos-26-intel` (x64) or the exact arm64 `macos-26` label. The pinned image docs report image versions `20260824.0517.1` and `20260907.0351.1` respectively. `macos-latest` currently aliases the arm64 row; it is not an architecture-neutral alias.
- Accepted Xcode 27 candidate: `xcode-27` (arm64; `xcode-27-xlarge` for the larger variant) is explicitly marked public preview. Its image README title is macOS 27 and image version `20260912.0186.1`; that does not create a `macos-27` label.
- Ubuntu 26.04 is also explicitly public preview (`ubuntu-26.04`, `ubuntu-26.04-arm`). It is not a baseline replacement for Ubuntu 24.04 without a policy decision.

## Image versus runner software

`actions/runner-images` publishes VM image definitions and image versions. `actions/runner` publishes the runner application. Its pinned release README states that the runner is the application that runs a workflow job and is used by hosted virtual environments or self-hosted runners. The latest release observed is `v2.337.0`; do not treat an image version such as `20260907.300.1` as a runner software version, and do not infer that hosted images run exactly `v2.337.0` from this source set.

## Capability/access unknowns

- Label existence is source-proven at the pinned public repository; actual eligibility for this Velnor GitHub account was not tested.
- Arm64, `large`, and `xlarge` labels may depend on repository/organization plan and hosted-runner entitlement. Access is **unknown**, not assumed.
- No workflow dispatch, hosted job, checks, or logs were fetched. Therefore scheduling, quota, capacity, billing, and runtime availability are **unknown**.
- No silent architecture fallback is authorized. A requested unsupported/inaccessible label must fail explicitly or be represented as an unresolved capability; it must not be rewritten to another architecture.

## Owner findings

- `g1_hosted_config`: encode the exact label/architecture matrix above; keep Ubuntu 24.04 and macOS 26 as baselines; gate `xcode-27` as an explicit arm64 public-preview policy choice; never invent `macos-27`.
- `g2native`: treat hosted label capability as an input contract. Preserve explicit arm64 labels (`ubuntu-24.04-arm`, `macos-26`, `windows-11-arm`) and fail closed when account access is unknown; do not fall back to x64.
- `root`: the source refresh proves labels and image/software separation only. Hosted account access and real job proof remain open.

## Evidence and limitations

- Raw GitHub API responses with headers are under `raw/`; decoded source documents and API content metadata retain the exact source commit/blob hashes.
- `runner-images-content-index.json`, `runner-content-index.json`, and `decoded-content-sha256.txt` preserve content identity. `manifest.sha256` covers the complete evidence directory.
- All API calls were read-only and returned HTTP 200; no source/workflow files, dispatches, merges, cancellations, checks, or logs were touched.
- This is a point-in-time public-source snapshot. GitHub-hosted capacity, entitlement, and label routing can change after capture.
