# G0 native-routing review — d83b7e20

Review date: 2026-09-20 (Asia/Ho_Chi_Minh)

Status: **NOT PASSED for full-fleet/latest-policy approval.** The typed
Apple-contract implementation at this revision is conditionally sound for
the historical macOS 26 arm64 contract, but it cannot claim current native
fleet coverage or the new latest-macOS policy.

This is a read-only review. No Velnor source, consumer source, generated
workflow, host installation, Docker runtime, publication, merge, or remote
write was performed.

## Scope and exact source

- Review tree: `dual-lane-native-policy`
- Branch: `codex/github-first-native-policy`
- Reviewed revision: `d83b7e20f43f55c7d678344c035989003a3fca9c`
  (`test(native): align verified Apple routing contracts`)
- Comparison/base: `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`
- Worktree: clean at review completion
- Reviewed code: `crates/velnor-workflow/src/native_contract.rs`,
  `swift_capability.rs`, legacy/S2 Swift scanning and routing, provider
  capability tables, runtime product defaults, and generated consumer
  dry-runs.

## Current official GitHub-hosted matrix

The official [GitHub-hosted runner matrix](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)
currently lists `macos-26-intel` as Intel and lists `macos-26`/`macos-latest`
as arm64; it also lists `xcode-27` as an arm64 public-preview label. The
official [runner-images matrix](https://github.com/actions/runner-images/blob/main/README.md)
links `xcode-27` to the `xcode-27-arm64` image.

The current [xcode-27-arm64 image README](https://github.com/actions/runner-images/blob/main/images/macos/xcode-27-arm64-Readme.md)
reports:

- image `20260912.0186.1`, OS `macOS 27.0 (26A5406e)`, kernel
  `Darwin 27.0.0`;
- Xcode `27.0` and macOS SDK `macosx27.0`;
- arm64-specific paths/tooling (for example `JAVA_HOME_*_arm64` and
  `/usr/local/share/chromedriver-mac-arm64`).

The current [macos-26-arm64 image README](https://github.com/actions/runner-images/blob/main/images/macos/macos-26-arm64-Readme.md)
reports macOS 26.6.2, Xcode 26.6, and SDKs through 26.5. The official
`macos-26-intel` entry is therefore an Intel macOS 26 image, not a fallback
equivalent for the newer macOS 27 arm64 preview. There is no official
macOS-27 Intel label in the current matrix.

Conclusion for the requested policy: the PR957 matrix claim is verified as
of this review (`xcode-27` = arm64/macOS 27 public preview;
`macos-26-intel` = Intel/macOS 26). A native Intel workload must not be
silently routed to `macos-26-intel` when “latest available macOS, including
preview” is mandatory. It needs an explicit unsatisfied-capability failure
until an Intel macOS 27 offer exists or policy explicitly grants an
exception.

## What passed

### Typed contract and executable architecture check

- `HostedAppleOffer` rejects arbitrary labels instead of treating a
  `runs-on` string as proof of Apple capability.
- The historical offer table distinguishes execution architecture from
  compiler output architectures for `macos-26` and `macos-26-intel`.
- The generated preflight checks required commands, selected developer
  directory, Xcode version, SDK listing/path/version, and `uname -m` against
  the required execution architecture.
- The hostile wrong-architecture fixture passes for arm64 and fails after
  the fake host changes to `x86_64`. This proves execution architecture is
  not being inferred from cross-build support.

### Consumer dry-runs

Read-only generation was exercised against all three native consumers:

| Consumer | Native evidence | Dry-run route | Review result |
| --- | --- | --- | --- |
| `tailrocks/tablerock` | `.macOS(.v26)`, AppKit/SwiftUI, Xcode project, XCFramework, explicit arm64/x86_64 build output | SwiftPM and Xcode units select `macos-26` | Historical arm64 route is typed; no live hosted build or XCFramework materialization proof |
| `parallax-telemetry-playground` | `.macOS(.v13)`, Darwin/MachO/MetricKit/os imports | Swift package selects `macos-26` | Historical route is typed; no live hosted build proof |
| `jackin-project/jackin` | `.macOS(.v26)`, Swift 6.2, local `JackinUsage.xcframework`; root manifest excluded and restored by manual config | Native Swift units select `macos-26`; rendered preflight carries macOS 26, SDK 26, Xcode 26.6, Swift 6.2, arm64 | Routing is present; excluded-manifest/XCFramework slice contract remains unverified |

The Jackin root `native/Package.swift` and `native/project.yml` were inspected
separately. The root package is excluded from generic scanning, and the
manual row declares `xcframework` plus an FFI dependency; no scanner evidence
currently inspects the XCFramework bundle's `Info.plist`, platform variants,
or actual Mach-O slices.

## Findings blocking approval

### H1 — d83 does not implement the current latest-macOS policy

`native_contract.rs` accepts only `macos-26` and `macos-26-intel`, with
host/Xcode/SDK facts for macOS 26. The S2 selector is hard-coded to
`MACOS_HOSTED_RUNS_ON = "macos-26"`. The current official public-preview
offer is `xcode-27` on macOS 27 arm64, but d83 has no verified offer for it.

Required correction: extend the single producer-backed hosted-image
attestation/offer table to the current official image, including exact label,
OS major, architecture, Xcode, and SDK facts; add a hostile fixture proving
that an unsupported newer image cannot silently fall through to an older
label. If the latest image is incompatible with a source minimum, generation
must fail explicitly.

### H2 — S2 cannot preserve Intel native execution

`crates/velnor-workflow/src/s2/provider.rs` has only `MacosArm64`; GitHub
provider capabilities expose only `native_macos_arm64`; and S2 Swift scanner
rows force `Platform::MacosArm64`. An explicit x86_64 execution contract is
therefore unrepresentable in S2 and is rejected/overridden rather than routed
to a typed Intel platform.

The universal `build_arches` compiler probe does not prove Intel execution.
The legacy offer's `macos-26-intel` entry does not repair S2, and using it as a
fallback would violate the new latest-OS policy anyway. Preserve Intel by
adding a real typed route with a verified current image, or fail with an
actionable unsupported-capability error. Do not drop Intel support through a
documentation-only waiver.

### M1 — scanner collapses non-iOS Apple SDK families to macOS

`swift_capability::manifest_native_contract` selects `IosSimulator` only for
`.iOS(` and maps every other Apple platform declaration to `Macos`. The
scanner separately recognizes tvOS, watchOS, visionOS, and macCatalyst, so a
source can be known to be Apple-native while its SDK family is wrong. The
generated preflight can then check `macosx` for a tvOS/watchOS/visionOS
consumer and fail late in the build instead of rejecting the contradictory
contract during generation.

Required correction: model each supported Apple SDK family, or reject an
unsupported family before rendering. Add hostile fixtures for every detected
non-macOS family.

### M2 — XCFramework detection is marker-only

`manifest_uses_xcframework` detects a `.binaryTarget` argument containing the
string `.xcframework`; it does not inspect bundle existence, `Info.plist`,
platform identifiers, library type, or Mach-O architecture slices. This is
especially material for Jackin's excluded root manifest and for Tablerock's
ignored/generated `target/xcframework` output. A generated macOS job can pass
host preflight while its actual consumer artifact is absent, platform-wrong,
or arm64-only when Intel output is required.

Required correction: require an explicit producer/materialization edge and
attest the materialized bundle's platform/architecture census before the
consumer runs. Keep the source marker as routing evidence only, never as
artifact proof.

### M3 — hostile preflight coverage does not cover missing/malformed facts

The executable preflight rejects a missing command and wrong `uname -m`, but
`require_version` is a no-op when a minimum/exact value is empty. A default
contract can therefore accept an empty or malformed `sw_vers -productVersion`
result. SDK version output is required to be nonempty, but its format/validity
is not checked when no version constraint is present. Existing tests cover the happy path and wrong
execution architecture, not missing/malformed macOS, SDK, or provider facts.

Required correction: validate nonempty, numeric version facts independently of
whether a source minimum exists, then apply minimum/exact constraints. Add
hostile fixtures for empty, malformed, contradictory, and stale provider
facts.

### M4 — historical `macos-15` defaults remain in product/runtime paths

Both legacy and S2 runtime-product constants still set
`MACOS_ARM64_RUNNER = "macos-15"`, and multiple profile/release fixtures
retain `macos-15`. The new policy forbids an old-OS fallback. d83's verified
offer table intentionally rejects `macos-15`, which causes the stale migration
fixtures to fail rather than proving a current route. This is a required
policy migration, not permission to re-enable the old label.

## Verification run

From the clean d83 tree:

- `rtk cargo test -p velnor-workflow native_contract --all-features`: **4
  passed**.
- `rtk cargo test -p velnor-workflow swift_capability --all-features`: **6
  passed**.
- `rtk cargo test -p velnor-workflow scanner_marks_xcframework --all-features`:
  **2 passed**.
- `rtk cargo fmt --all -- --check`: **passed**.
- `rtk cargo check -p velnor-workflow --all-features`: **passed**.
- `rtk cargo clippy -p velnor-workflow --lib --all-features -- -D warnings`:
  **passed**.
- `rtk cargo clippy -p velnor-workflow --all-targets --all-features -- -D
  warnings`: **passed**.
- `rtk cargo test -p velnor-workflow --all-features`: **1744 passed, 5
  failed**. All five failures are stale migration fixtures selecting
  `macos-15`; the new native contract correctly reports no verified offer and
  asks for `macos-26` or `macos-26-intel`.

No full hosted install, native build, upgrade, XCFramework producer run,
Mach-O census, or publication was executed. The focused checks prove source
and routing behavior only; they do not prove a deliverable native product.

## Review disposition

The historical d83 arm64/macOS-26 routing change is a useful typed-contract
checkpoint and its wrong-architecture preflight test is valid. It is **not a
current full-fleet approval**: latest preview routing, Intel preservation,
SDK-family correctness, artifact provenance, and hostile missing-fact checks
remain unresolved. Do not claim G3/native delivery or use Intel macOS 26 as a
silent fallback for the current macOS 27 arm64 latest image.
