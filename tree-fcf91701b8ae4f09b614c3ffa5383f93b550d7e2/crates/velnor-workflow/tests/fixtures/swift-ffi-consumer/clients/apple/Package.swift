// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "Widget",
    targets: [
        // Neutral XCFramework consumer: the binary target binds this package
        // to an Apple executor, mirroring an FFI bridge over a packed
        // framework. The scan reads the manifest text only; the bundle path
        // is never resolved during generation.
        .binaryTarget(
            name: "WidgetFFI",
            path: "../target/xcframework/Widget.xcframework"
        ),
        .target(name: "Widget", dependencies: ["WidgetFFI"]),
        .testTarget(name: "WidgetTests", dependencies: ["Widget"]),
    ]
)
