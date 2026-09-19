# G3 native-routing audit (read-only)

Audit date: 2026-09-19 (Asia/Ho_Chi_Minh)

Scope: tailrocks/tablerock, tailrocks/parallax-telemetry-playground,
jackin-project/jackin, Velnor scanner/routing, and the hosted macOS image
contract. No source repository, generated workflow, or generator output was
edited. The three source repositories were shallow-cloned under
../native-routing-sources/ for inspection only.

## Authority and worker metadata

- Goal specification: velnor-github-first-dual-lane-goal.md, especially
  §§7 and “Platform and provider contract” (L182-L236). It requires platform
  detection from code/dependencies, native Apple checks for all three projects,
  central scanner/routing fixes, and validation against the actual hosted
  image.
- Velnor main under audit:
  abe9ad82a2d4d01b706bbc6122ab6ccb150faad9 (main, merge PR #951). The
  working tree only had the user-owned untracked goal specification; it was
  not touched.
- Session metadata was independently read from
  /Users/donbeave/.codex-chainargos/state_5.sqlite and
  ../dual-lane-evidence/session.json: orchestrator/root gpt-6-astra,
  effort low; G0 fleet worker gpt-5.6-luna, effort max; both point at
  Velnor abe9ad82..., branch main, same workspace.
- The investigate-first skill was applied: evidence first, separate enabling
  condition from symptom, and no implementation in this audit.

## Exact source refs

| repository | default branch | inspected source SHA | commit |
| --- | --- | --- | --- |
| tailrocks/tablerock | main | e2fe040c9d9cbf0eb3994d8b6cdffd0a772de6fc | chore: sync velnor-workflow to b9c3156 (#79) |
| tailrocks/parallax-telemetry-playground | main | 54d09bf71181dcfc72d6829fabec3c53f55aacf9 | chore: sync velnor-workflow to b9c3156 (#49) |
| jackin-project/jackin | main | 665f7e3735c1f76ce5ee8a9e27c676381a474cfb | fix(ci): include cargo:sccache in desktop cadence mise tools |

Permalinks:

- [Tablerock main source](https://github.com/tailrocks/tablerock/tree/e2fe040c9d9cbf0eb3994d8b6cdffd0a772de6fc)
- [Playground main source](https://github.com/tailrocks/parallax-telemetry-playground/tree/54d09bf71181dcfc72d6829fabec3c53f55aacf9)
- [Jackin main source](https://github.com/jackin-project/jackin/tree/665f7e3735c1f76ce5ee8a9e27c676381a474cfb)
- [Velnor audit source](https://github.com/tailrocks/velnor3/tree/abe9ad82a2d4d01b706bbc6122ab6ccb150faad9)

## Decision

The present Tablerock and playground Swift jobs are incorrectly routed to
ubuntu-24.04. The source contracts are Darwin-native; Linux Swift compiler
availability is not evidence of AppKit, SwiftUI, Darwin, MetricKit, Xcode, or
XCFramework correctness.

Jackin's native jobs are already routed to macos-26 in generated workflows,
but that correctness is typed/manual and partially hidden by the scanner
exclusion of native/Package.swift. Preserve that explicit contract while
centralizing equivalent source-derived detection for repositories that do not
declare it manually.

Velnor is Linux-container-only. Every Apple-bound unit is a platform exclusion
from the Velnor lane, not a successful Velnor execution. Portable
Rust/Bun/Gradle/Docker/metadata checks remain eligible when the repository
policy enables that lane.

## Unit capability matrix

| repository/unit | source evidence | required executor | current generated result | Velnor |
| --- | --- | --- | --- | --- |
| Tablerock swift-package-native | native/Package.swift declares .macOS(.v26), system library, cargo-linked library, SwiftUI/AppKit sources | GitHub-hosted macOS; Swift 6.2/Xcode 26.6 or compatible SDK; FFI product must exist | github job is ubuntu-24.04 | Exclude: Darwin-native |
| Tablerock swift-xcodeproj-tablerock | XcodeGen project; every target platform macOS; deployment 26.0; Xcode 26.6; AppKit/SwiftUI; XCFramework dependency | GitHub-hosted macOS with xcodebuild; macOS SDK 26.x; FFI XCFramework producer in same job | github job is ubuntu-24.04 | Exclude: Darwin-native |
| Tablerock Rust/mise/policy units | Cargo workspace, Rust tests/binaries/build scripts and policy files | Linux hosted; same commands can be containerized | Generated as ordinary Linux units | Eligible, subject to configured lane |
| Playground swift-package-macos | .macOS(.v13) plus Darwin, MachO, MetricKit, os imports and sysctl/Mach-O/thermal APIs | GitHub-hosted macOS SwiftPM; macOS 13+ API contract | github job is ubuntu-24.04 | Exclude: Darwin-native |
| Playground Rust/Bun/Gradle/Docker/etc. | Separate generated units under Rust/Bun/Gradle/Docker roots | Linux hosted/container-compatible | Generated on GitHub and Velnor where enabled | Eligible; preserve both-lane coverage |
| Jackin swift-package-native | .macOS(.v26) and local JackinUsage.xcframework; explicit capabilities=xcframework and FFI dependency | GitHub-hosted macOS 26/Xcode 26.6; run FFI producer then SwiftPM | Generated reusable Swift workflow is macos-26 | Exclude: Darwin-native |
| Jackin prototype package | .macOS(.v26) and AppKit/SwiftUI source; explicit capabilities=xcode | GitHub-hosted macOS/Xcode | Generated reusable Swift workflow is macos-26 | Exclude: Darwin-native |
| Jackin desktop cadence/release | XcodeGen macOS project, strict Swift 6, signing/notarization and artifact release jobs | GitHub-hosted macOS 26; credentials only for publish job | Existing cadence/release jobs use macOS | Exclude: Darwin-native/signing |
| Jackin Rust/Bun/Docker/docs | Ordinary non-Apple units | Linux hosted/container-compatible | Current policy intentionally runners=github | Workload-eligible in principle, but current policy does not admit Velnor |

## Tablerock evidence

- native/Package.swift uses Swift tools 6.2 and declares only .macOS(.v26)
  ([source lines 1-17](https://github.com/tailrocks/tablerock/blob/e2fe040c9d9cbf0eb3994d8b6cdffd0a772de6fc/native/Package.swift#L1-L17)).
  It links tablerock_ffi through a Cargo release path and exposes an
  executable app plus tests (lines 18-85).
- Swift sources import AppKit/SwiftUI/CoreFoundation/Security. This is native
  API usage, not a portable SwiftPM package.
- native/App/project.yml requires XcodeGen 2.46, Xcode 26.6, macOS deployment
  26.0, Swift 6.0 strict concurrency, and Release arm64 x86_64 builds
  ([lines 6-28](https://github.com/tailrocks/tablerock/blob/e2fe040c9d9cbf0eb3994d8b6cdffd0a772de6fc/native/App/project.yml#L6-L28)).
  All app/framework/test targets are platform macOS; the application links
  SystemConfiguration, CoreFoundation, Security, iconv, and
  target/xcframework/tablerock_ffiFFI.xcframework
  ([lines 61-99](https://github.com/tailrocks/tablerock/blob/e2fe040c9d9cbf0eb3994d8b6cdffd0a772de6fc/native/App/project.yml#L61-L99)).
- scripts/build-xcframework.sh builds Rust for aarch64-apple-darwin and
  x86_64-apple-darwin, lipo-combines them, then invokes
  xcodebuild -create-xcframework
  ([lines 1-27](https://github.com/tailrocks/tablerock/blob/e2fe040c9d9cbf0eb3994d8b6cdffd0a772de6fc/scripts/build-xcframework.sh#L1-L27),
  [lines 64-91](https://github.com/tailrocks/tablerock/blob/e2fe040c9d9cbf0eb3994d8b6cdffd0a772de6fc/scripts/build-xcframework.sh#L64-L91)).
- The committed Xcode project consumes that bundle, but the bundle is under
  ignored target/; a clean checkout has no XCFramework. The generated Xcode
  unit has no producer/preparation edge in the old output. This is a real
  workflow/product prerequisite, separate from runner selection.
- The source config is schema 1 and pins generator revision b9c3156; its
  default GitHub runner is ubuntu-24.04 and it has no macOS runner override
  (.github-gen/velnor-workflow.toml, lines 1-18). Current generated evidence:
  project.toml identifies swift-package-native and
  swift-xcodeproj-tablerock at lines 157-182 but sets
  github_runner = ubuntu-24.04 at line 15. ci-unit-swift.yml line 104 runs
  the GitHub Swift reusable job on ubuntu-24.04.
- Correct migration: make both Swift units Apple-bound. Keep Rust and other
  portable units Linux/container eligible. Add a same-job FFI/XCFramework
  producer or explicit prerequisite graph before Xcode/SwiftPM consumers; do
  not claim a clean-clone native pass until that edge is exercised.

## Playground evidence

- macos/Package.swift uses Swift tools 5.9 and declares .macOS(.v13)
  ([lines 1-17](https://github.com/tailrocks/parallax-telemetry-playground/blob/54d09bf71181dcfc72d6829fabec3c53f55aacf9/macos/Package.swift#L1-L17)).
- NativeContext.swift imports Darwin, MachO, MetricKit, and os, then reads
  thermal state, sysctl data, process identity, Mach-O UUID, and MetricKit
  payloads
  ([lines 1-8](https://github.com/tailrocks/parallax-telemetry-playground/blob/54d09bf71181dcfc72d6829fabec3c53f55aacf9/macos/Sources/MacOSPlayground/NativeContext.swift#L1-L8),
  [lines 24-58](https://github.com/tailrocks/parallax-telemetry-playground/blob/54d09bf71181dcfc72d6829fabec3c53f55aacf9/macos/Sources/MacOSPlayground/NativeContext.swift#L24-L58)).
  The package is therefore not a Linux Swift smoke test.
- The source config is schema 1, pins b9c3156, enables both runners, and
  chooses ubuntu-24.04 as GitHub default. It manually declares
  swift-package-macos, but without an Apple platform requirement.
- Current generated project.toml detects swift-package:macos and emits
  swift-package-macos at lines 240-248, while its workflow default remains
  github_runner = ubuntu-24.04 (line 15). ci-unit-swift.yml line 104
  consequently runs this package on Ubuntu.
- Correct migration: route only swift-package-macos to hosted macOS and
  remove it from the Velnor execution set. Keep the Rust workspace, Bun web
  checks, Gradle, Docker, and portable metadata units in both lanes as
  configured. A native-only exclusion must be visible in coverage and cannot
  satisfy a Velnor success claim.

## Jackin evidence

- native/Package.swift uses Swift tools 6.2, .macOS(.v26), and a local
  JackinUsage.xcframework binary target
  ([lines 1-30](https://github.com/jackin-project/jackin/blob/665f7e3735c1f76ce5ee8a9e27c676381a474cfb/native/Package.swift#L1-L30)).
- native/project.yml records the shipping contract: minimum macOS 26.0,
  Xcode 26.6, macOS 26.5 SDK, Swift 6 strict concurrency, arm64 only, and a
  nonblocking Xcode 27 forward lane
  ([lines 3-33](https://github.com/jackin-project/jackin/blob/665f7e3735c1f76ce5ee8a9e27c676381a474cfb/native/project.yml#L3-L33)).
  All targets are macOS; app/tests consume the same XCFramework
  ([lines 35-50](https://github.com/jackin-project/jackin/blob/665f7e3735c1f76ce5ee8a9e27c676381a474cfb/native/project.yml#L35-L50),
  [lines 85-142](https://github.com/jackin-project/jackin/blob/665f7e3735c1f76ce5ee8a9e27c676381a474cfb/native/project.yml#L85-L142)).
- The root native manifest is excluded from generic scan
  (.github-gen/velnor-workflow.toml lines 9-10), then restored by typed
  config as swift-package-native, capabilities=xcframework, and
  depends_on=rust-jackin-usage-ffi (lines 146-162). The prototype package
  has .macOS(.v26) and the config overrides its scanned unit with
  capabilities=xcode (lines 170-175).
- Existing generated correctness is visible in .github/workflows/ci-pr.yml:
  both Apple Swift jobs pass apple_executor=true (lines 2565-2601), and
  reusable ci-unit-swift.yml line 120 uses runs-on: macos-26. This is the
  desired routing outcome, but generated project.toml does not serialize the
  explicit capability on the merged root unit; retain source config as the
  authoritative typed override during schema migration.
- Desktop merge/scheduled profiles run on macos-26, set
  DEVELOPER_DIR=/Applications/Xcode_26.6.app/Contents/Developer and
  MACOSX_DEPLOYMENT_TARGET=26.0, and install Rust/nextest/sccache/boltffi/
  XcodeGen/SwiftLint/xcbeautify (config lines 57-87). Release build and sign
  jobs are also runner=macos with the same toolchain environment (lines
  191-220). Preserve these native responsibilities.
- Correct migration: retain Apple routing and typed FFI prerequisite; central
  scanner should independently recognize .macOS(.v26), XCFramework targets,
  and Apple imports when a future schema/config does not hide the root
  manifest. Keep Jackin's Velnor exclusion for native jobs and current
  GitHub-only policy for its container jobs unless policy is intentionally
  changed.

## Enabling condition in Velnor

The bug class is “Swift kind defaults to portable Linux, while the scanner
cannot see Apple platform/API requirements.” It is not an Ubuntu label typo.

- Legacy scanner crates/velnor-workflow/src/scan/swift.rs assigns every
  SwiftPM package PlatformRequirement::swift_package() at lines 15-55.
  swift_package() is explicitly portable in
  crates/velnor-workflow/src/platform.rs lines 64-74.
  ([scanner source](https://github.com/tailrocks/velnor3/blob/abe9ad82a2d4d01b706bbc6122ab6ccb150faad9/crates/velnor-workflow/src/scan/swift.rs#L15-L55),
  [platform source](https://github.com/tailrocks/velnor3/blob/abe9ad82a2d4d01b706bbc6122ab6ccb150faad9/crates/velnor-workflow/src/platform.rs#L64-L74)).
- The legacy scanner detects only two native overlays:
  xcode_scheme_units assigns apple_xcode() (lines 68-184), and a manifest
  containing both .binaryTarget and .xcframework assigns apple_xcframework()
  (lines 187-204). It does not inspect Package.platforms, Apple framework
  imports, linker settings, or system-framework references.
- The schema-2 scanner has the same structural gap: its SwiftPM default is
  the portable/default unit (crates/velnor-workflow/src/s2/scan/swift.rs,
  lines 15-55), while Xcode schemes become MacosArm64 plus
  native_macos_arm64 (lines 67-184), and XCFramework manifests get the same
  overlay (lines 186-204).
  ([schema-2 scanner source](https://github.com/tailrocks/velnor3/blob/abe9ad82a2d4d01b706bbc6122ab6ccb150faad9/crates/velnor-workflow/src/s2/scan/swift.rs#L15-L55)).
- Routing itself is capability-correct once the unit is typed:
  PlatformRequirement::requires_apple() checks macOS/xcode/XCFramework
  (platform.rs lines 76-104), executors provide Linux and macOS for GitHub
  but Linux only for Velnor (lines 155-226), and
  github_runner_for_unit() selects the macOS label for Apple requirements
  (lines 238-250). The central fix belongs in scanner/platform requirement
  derivation and generated lane admission, not repository-local Ubuntu
  strings.
  ([executor/routing source](https://github.com/tailrocks/velnor3/blob/abe9ad82a2d4d01b706bbc6122ab6ccb150faad9/crates/velnor-workflow/src/platform.rs#L76-L104)).
- Target repositories are stale schema-1 generated outputs: Tablerock and
  playground pin b9c3156; Jackin pins 06050c9. Velnor main at abe9ad82...
  contains newer schema-2 generator revision
  fdeed261bd2247a38db6922a7726cd45d3d6f31e, but no target output was
  regenerated during this audit.

## Generic central-fix task

Implement one generator change and publish one immutable generator revision,
then regenerate every affected consumer:

1. Extend Swift package capability derivation to inspect, without executing
   project code:
   - manifest platforms declarations (.macOS, .iOS, etc.);
   - Apple-only target imports/frameworks (AppKit, SwiftUI, Darwin, MachO,
     MetricKit, CoreGraphics, os, and linked Apple frameworks);
   - local/remote binary targets and .xcframework references;
   - XcodeGen/project settings (SDKROOT, deployment target, target platform,
     architecture), shared schemes, and test destination.
2. Emit a provider-neutral Apple requirement (os=macos, architecture where
   proven, and typed SDK capabilities). Do not overload xcode for every
   SwiftPM package: a macOS SwiftPM target needs a macOS executor even when
   its command remains swift build/test; an Xcode scheme needs xcode; an
   XCFramework consumer needs xcframework.
3. Preserve portable SwiftPM behavior when no Apple platform/API/linkage
   evidence exists. Explicit typed config (os, arch, capabilities) remains
   the override for dynamic or intentionally excluded manifests, and must
   merge predictably with scanner evidence rather than silently weakening it.
4. Resolve each unit separately per lane/provider. Apple units run on the
   configured GitHub macOS label and are explicitly absent from Velnor's
   Linux execution graph. If no enabled provider can satisfy an Apple
   requirement, generation must fail with unit, requirement, provider, and
   remedy.
5. Preserve product edges. Tablerock and Jackin Swift consumers need an
   in-job or explicitly materialized FFI/XCFramework prerequisite because
   GitHub jobs do not share target/. A generated job that merely changes
   runs-on is not sufficient.
6. Regenerate from the published revision, verify byte-stable regeneration,
   and remove stale schema-1 output only through the supported migration path;
   do not hand-edit generated YAML.

## Required fixtures and assertions

Add scanner + routing + rendering fixtures, not snapshot-only YAML tests:

- portable SwiftPM package (Foundation only, no platform declaration) =>
  Linux/default executor and Velnor eligible;
- SwiftPM .macOS(.v13/.v26) package with AppKit/SwiftUI/Darwin/MetricKit =>
  macOS hosted only, Velnor excluded;
- .binaryTarget/.xcframework package => macOS plus xcframework capability,
  with missing-product failure;
- Xcode macOS project/shared scheme (SDKROOT=macosx, deployment target,
  explicit arm64/x86_64) => macOS/Xcode;
- Xcode iOS project/shared scheme (SDKROOT=iphoneos) => macOS/Xcode with
  iOS simulator destination, never Linux;
- Apple system-framework/linker settings without obvious imports => macOS;
- explicit os=macos/capability override on a scanned package (Jackin
  pattern) => override retained in schema-2 output and generated job;
- both-lane config => portable unit appears in GitHub and Velnor graphs while
  Apple unit appears only in GitHub macOS graph and is counted as a platform
  exclusion, not a Velnor pass;
- missing macos_runner or unsupported Apple capability => generation error;
- producer/consumer fixture => FFI/XCFramework producer runs before consumer
  in one job or a declared artifact boundary; clean clone cannot resolve a
  bundle from ignored target/;
- generated assertions must inspect runs-on, lane admission, unit
  dependencies, command graph, and exclusion reasons.

## Hosted image/toolchain verification

Authoritative references checked:

- [GitHub-hosted runners reference](https://docs.github.com/en/actions/reference/runners/github-hosted-runners):
  current public labels include arm64 macos-14, macos-15, macos-26, x64
  macos-15-intel/macos-26-intel, and public-preview xcode-27; Linux includes
  ubuntu-24.04 and ubuntu-26.04.
- [macOS 26 runner image README](https://github.com/actions/runner-images/blob/main/images/macos/macos-26-Readme.md):
  current image is macOS 26.6.1; default Xcode is 26.6 at
  /Applications/Xcode_26.6.app, and the image includes macOS 26.0/26.1/
  26.2/26.4/26.5 SDKs. Xcode 26.5/26.6 expose the macOS 26.5 SDK.
- [macOS 26 toolset JSON](https://github.com/actions/runner-images/blob/main/images/macos/toolsets/toolset-26.json):
  confirms default Xcode 26.6 (26.6+17F113) and architecture entries.
- [runner-images repository labels](https://github.com/actions/runner-images):
  macOS 26 arm64 labels include macos-26; x64 labels include
  macos-26-intel; Xcode 27 is a separate preview image.

Capability conclusions:

- Jackin's shipping requirement (macOS 26.0, Xcode 26.6, macOS 26.5 SDK,
  Swift 6, arm64) is hosted-image compatible on macos-26. Set DEVELOPER_DIR
  explicitly as the repository already does.
- Tablerock's Swift/Xcode minimum (macOS 26.0, Swift tools 6.2/Xcode 26.6)
  is compatible on macos-26. Its universal Release artifact explicitly
  contains arm64 and x86_64; an arm64 runner can cross-build but does not
  prove x86_64 execution. Use macos-26-intel if a native x86_64 execution
  check is required.
- Playground requires only macOS 13 APIs and Swift tools 5.9; current
  macOS-26 arm64 is a newer compatible host, subject to compile/test
  validation. Its use of MetricKit/sysctl/Mach-O must remain a real macOS run.
- No hosted-image capability blocker was found for the declared shipping
  requirements. Actual blockers are wrong Ubuntu routing, missing clean-clone
  XCFramework prerequisite in Tablerock, and any unverified architecture or
  SDK assumptions.
- macOS 26 standard arm64 resources are documented as 7 GB RAM / 14 GB SSD
  in the runner-image README. Record resource pressure during native
  workflows; do not weaken deployment/toolchain requirements to fit it.
- Xcode 27 is public preview. Keep Jackin's forward lane nonblocking until
  explicit policy/image verification makes it required; do not treat a preview
  image as proof of shipping Xcode 26.6 parity.

Docs-verification gates for the implementation run:

- At job start assert uname -m, sw_vers, xcode-select -p,
  xcodebuild -version, xcodebuild -showsdks, and the selected deployment
  target. A moving hosted label is not proof by itself.
- Verify macos-26 resolves arm64 and macos-26-intel is available before
  claiming native x86_64 execution for Tablerock. Cross-compilation from
  arm64 is not x86_64 runtime coverage.
- Verify the exact Xcode 26.6 developer directory exists before honoring
  Jackin's shipping contract. If absent, record a hosted-image blocker; do
  not lower the declared Xcode/SDK requirement.
- Verify rustup Apple targets, cargo-xtask/boltffi tooling, XcodeGen, and
  enough disk/RAM after checkout before claiming FFI/XCFramework production.
  These are execution checks; runner documentation alone cannot prove them.

## Audit stop condition

This is a read-only research handoff. No rollout, PR, generator release,
target regeneration, or generated edit was performed. A later implementation
worker needs the generic scanner/platform fix, fixture coverage, immutable
generator publication, consumer regeneration, and actual hosted runs before
G3 can be considered passed.
