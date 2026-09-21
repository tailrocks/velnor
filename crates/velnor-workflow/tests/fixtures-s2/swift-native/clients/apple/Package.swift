// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "BridgeClient",
    platforms: [.macOS(.v15)],
    targets: [
        .binaryTarget(
            name: "BridgeCoreFFI",
            path: "../../target/xcframework/BridgeCore.xcframework"
        ),
        .target(
            name: "BridgeClient",
            dependencies: ["BridgeCoreFFI"]
        ),
        .testTarget(
            name: "BridgeClientTests",
            dependencies: ["BridgeClient"]
        ),
    ]
)
