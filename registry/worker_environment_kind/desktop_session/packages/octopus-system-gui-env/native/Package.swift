// swift-tools-version:6.2
import PackageDescription
import class Foundation.ProcessInfo

precondition(ProcessInfo.processInfo.environment["SWIFTCI_USE_LOCAL_DEPS"] == nil,
             "Native desktop dependencies must use the checked-in remote resolution, not sibling checkouts")

let package = Package(
    name: "octopus-desktop-native",
    platforms: [.macOS(.v14)],
    products: [
        .executable(name: "octopus-desktop-native", targets: ["OctopusDesktopNative"])
    ],
    dependencies: [
        .package(url: "https://github.com/swiftlang/swift-markdown.git",
                 revision: "75e3df1d7b664ef3c96595de36c243b98599b3bc")
    ],
    targets: [
        .target(name: "OctopusDesktopProducerBridge", path: "producer-bridge",
            sources: ["producer.mm"], publicHeadersPath: "include",
            cxxSettings: [.unsafeFlags(["-fobjc-arc"])],
            linkerSettings: [.linkedFramework("AppKit"), .linkedFramework("CoreServices"),
                .linkedFramework("QuartzCore")]),
        .executableTarget(
            name: "OctopusDesktopNative",
            dependencies: [.product(name: "Markdown", package: "swift-markdown"), "OctopusDesktopProducerBridge"],
            path: ".",
            exclude: ["build.cjs", "input-check.swift", "Package.resolved", "pip", "producer-bridge",
                "configure-transport.cjs", "configure-transport.test.cjs", "desktop.sb", "resources"],
            sources: [
                "input.swift", "channel.swift", "app-types.swift", "applications.swift", "accessibility.swift", "ax-attributed-text.swift", "ax-presentation.swift",
                "window-observer.swift", "app-capture.swift", "app-input.swift", "app-focus.swift", "app-cursor.swift",
                "pasteboard.swift", "paste-format.swift", "app-pip.swift", "app-pip-publisher.swift", "macOS.swift"
            ]
        )
    ],
    swiftLanguageModes: [.v5],
    cxxLanguageStandard: .cxx17
)
