# Native scanner review: `746745c56a6798a13d176341b4996f66cdee295`

Review date: 2026-09-20  
Repository: `dual-lane-native-routing`  
Base: `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`

## Result

Not approved for G3 native completion. The commit improves static SwiftPM Apple detection and its focused/full Rust tests pass, but the exact generated Tablerock surface still has material blockers.

## Findings

1. **P1 — required macOS/Xcode contract is not typed or selected.** `swift_capability.rs` returns only `{ apple, xcframework }`; `PlatformRequirement::apple_swift_package()` and `apple_xcframework()` use `os=macos`, `arch=any`, and no deployment/SDK/toolchain fields. Schema-2 overlays all Apple evidence as `Platform::MacosArm64` plus `native_macos_arm64`, with no Xcode-vs-XCFramework distinction. No code reads `Package.swift` deployment versions, `project.pbxproj`/XcodeGen `MACOSX_DEPLOYMENT_TARGET`, `SWIFT_VERSION`, `SDKROOT`, `ARCHS`, or `DEVELOPER_DIR`. The generated default remains `macos-15`, while Tablerock declares macOS 26.0, Swift tools 6.2, Xcode 26.6, and macOS 26 SDK. A macOS label alone does not satisfy that contract.

2. **P1 — Tablerock’s XCFramework producer edge is absent.** Tablerock’s `native/App/TableRock.xcodeproj/project.pbxproj` references `../../target/xcframework/tablerock_ffiFFI.xcframework`; the bundle is ignored and absent in a clean checkout. The exact scan dry-run produced `swift-package-native` and `swift-xcodeproj-tablerock` on `macos-15`, but neither unit has a `depends_on`/prerequisite or a preparation command for `scripts/build-xcframework.sh`/the FFI product. The new `.binaryTarget(... .xcframework ...)` parser only examines `Package.swift`; Tablerock’s SwiftPM manifest has no binary target and the Xcode project reference is not inspected for a typed `xcframework` requirement. `NamedProduct`/`Prerequisite` infrastructure exists but this commit does not materialize the edge.

3. **P1 — generator revision was not advanced.** Both legacy and schema-2 `GENERATOR_REVISION` constants remain `"52"`, although this commit changes scan evidence, platform placement, and generated workflow bytes. The source contract says every render-affecting change must bump this revision; retaining `52` lets stale generated state/runtime identity appear current and prevents the required immutable generator epoch from identifying this classifier.

4. **P2 — parser regression surface is incomplete.** Tests cover ordinary comments and quoted/multiline strings, but not Swift extended/raw strings, escaped multiline delimiters, whitespace/attribute variants of imports, or unrelated Swift files inside a package root. The hand-written lexer can classify text after a raw-string quote as code and `is_package_source` scans every `.swift` descendant outside nested Package.swift roots, so sibling Xcode/examples can leak Apple evidence into a portable package. `source_imports_apple_module` also only accepts bare `import Module`/three special attributes; valid `import class AppKit.NSView`, `@_exported import SwiftUI`, and several Apple frameworks absent from `APPLE_MODULES` can remain untyped. Add adversarial fixtures before relying on the classifier.

## Exact source evidence

- Velnor goal requires platform detection from actual code/dependencies, typed macOS/Xcode/Swift/SDK/deployment/architecture requirements, and an in-job or explicit FFI/XCFramework edge.
- Tablerock `native/Package.swift`: `.macOS(.v26)`, Swift tools 6.2, Cargo-linked FFI.
- Tablerock `native/App/project.yml`: Xcode 26.6, deployment 26.0, Release `arm64 x86_64`, and `target/xcframework/tablerock_ffiFFI.xcframework`.
- Tablerock `scripts/build-xcframework.sh`: builds FFI for both Apple triples and invokes `xcodebuild -create-xcframework`.

## Verification

- `rtk cargo test -p velnor-workflow --lib`: 1738 passed.
- `rtk cargo test -p velnor-workflow swift_capability --lib`: 3 passed.
- Exact commit dry-run against clean tracked Tablerock source produced 11 units; both Swift units rendered `runs-on: macos-15`; total dependency edges remained Rust-only (15), with no XCFramework producer edge.
