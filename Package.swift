// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "TouchNext",
    platforms: [.macOS(.v14)],
    targets: [
        .target(name: "TouchCore"),
        .target(name: "TouchPlatform", dependencies: ["TouchCore"]),
        .executableTarget(name: "TouchNextApp", dependencies: ["TouchCore", "TouchPlatform"]),
        .testTarget(name: "TouchCoreTests", dependencies: ["TouchCore"], resources: [.copy("Fixtures")]),
        .testTarget(name: "TouchPlatformTests", dependencies: ["TouchPlatform", "TouchCore"]),
    ]
)
