// swift-tools-version:5.9
// HQ Notch: a notch companion for Claude HQ. Builds with the Command Line Tools alone
// (no Xcode): `swift build -c release`, then notch/tools/install.py bundles the .app.
import PackageDescription

let package = Package(
    name: "HQNotch",
    platforms: [.macOS(.v14)],
    targets: [
        // Pure logic: creature assignment, hook protocol, session state. No AppKit.
        .target(name: "HQNotchCore", path: "Sources/HQNotchCore"),
        // The app: notch panel, socket server, sprites, terminal jump.
        .executableTarget(name: "HQNotch", dependencies: ["HQNotchCore"], path: "Sources/HQNotch"),
        // Assertion runner (XCTest/swift-testing are not shipped with the CLT).
        .executableTarget(name: "hqnotch-selftest", dependencies: ["HQNotchCore"], path: "Sources/SelfTest"),
    ]
)
