// swift-tools-version:5.10
import PackageDescription

let package = Package(
    name: "DSHLauncher",
    platforms: [.macOS(.v13)],
    products: [
        .executable(name: "DSHLauncher", targets: ["DSHLauncher"]),
    ],
    targets: [
        // Pure lifecycle logic (runtime install, daemon supervision, probes); no AppKit.
        .target(
            name: "DSHLauncherCore",
            path: "Sources/DSHLauncherCore"
        ),
        // AppKit menu bar shell: status item, windows, login item, deep links.
        .executableTarget(
            name: "DSHLauncher",
            dependencies: ["DSHLauncherCore"],
            path: "Sources/DSHLauncher"
        ),
        .testTarget(
            name: "DSHLauncherCoreTests",
            dependencies: ["DSHLauncherCore"],
            path: "Tests/DSHLauncherCoreTests"
        ),
    ]
)
