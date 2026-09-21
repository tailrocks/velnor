// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "BridgeCore",
    platforms: [
        .macOS(.v13),
    ],
    products: [
        .library(
            name: "BridgeCore",
            targets: ["BridgeCore"]
        ),
    ],
    targets: [
        .binaryTarget(
            name: "BridgeCoreFFI",
            path: "../../../../target/xcframework/BridgeCore.xcframework"
        ),
        .target(
            name: "BridgeCore",
            dependencies: ["BridgeCoreFFI"],
            path: "Sources"
        ),
    ]
)