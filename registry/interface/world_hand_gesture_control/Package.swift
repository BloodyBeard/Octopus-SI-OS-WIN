// swift-tools-version: 5.9

import PackageDescription

let package = Package(
    name: "AirHand",
    platforms: [
        .macOS(.v13)
    ],
    products: [
        .executable(name: "AirHand", targets: ["AirHand"])
    ],
    targets: [
        .executableTarget(
            name: "AirHand",
            path: "Sources/AirHand"
        )
    ]
)
