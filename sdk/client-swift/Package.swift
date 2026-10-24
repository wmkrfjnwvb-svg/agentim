// swift-tools-version:5.9
// AgentIM client SDK for iOS and macOS (docs/12 §2): the Rust core through
// its C ABI. Releases ship the core as an XCFramework (binaryTarget); here
// it links `libagentim_client` from sdk/client-rust (see README.md).
import PackageDescription

let package = Package(
    name: "AgentIM",
    platforms: [.macOS(.v14), .iOS(.v17)],
    products: [.library(name: "AgentIM", targets: ["AgentIM"])],
    targets: [
        .systemLibrary(name: "CAgentIM", path: "Sources/CAgentIM"),
        .target(name: "AgentIM", dependencies: ["CAgentIM"]),
        .executableTarget(name: "Conformance", dependencies: ["AgentIM"]),
    ]
)
