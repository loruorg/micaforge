// swift-tools-version: 6.0
//
// The Micaforge Swift SDK, published from the repository root so Swift Package Manager can
// add it straight from the repository URL (`https://github.com/loruorg/micaforge`). The
// sources live with the other SDKs under `packages/sdk-swift`; this manifest only points
// at them. No dependencies, by design: an analytics library has no business pulling a
// dependency tree into somebody's app.
import PackageDescription

let package = Package(
    name: "Micaforge",
    platforms: [
        .iOS(.v15),
        .macOS(.v12),
        .tvOS(.v15),
        .watchOS(.v8),
        .visionOS(.v1),
    ],
    products: [
        .library(name: "Micaforge", targets: ["Micaforge"]),
    ],
    targets: [
        .target(
            name: "Micaforge",
            path: "packages/sdk-swift/Sources/Micaforge"
        ),
        .testTarget(
            name: "MicaforgeTests",
            dependencies: ["Micaforge"],
            path: "packages/sdk-swift/Tests/MicaforgeTests"
        ),
    ],
    swiftLanguageModes: [.v6]
)
