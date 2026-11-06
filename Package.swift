// swift-tools-version: 6.0
// The Swift client of the daemon API: `RosettaClient`, under clients/swift (see clients/swift/README.md). The manifest
// sits at the repository root because SwiftPM resolves a package by its repository: a consumer pins a release tag of
// this repository, as npm consumers pin the TypeScript client's published version.

import PackageDescription

let package = Package(
  name: "rosetta",
  platforms: [.iOS(.v18), .macOS(.v15)],
  products: [
    .library(name: "RosettaClient", targets: ["RosettaClient"])
  ],
  targets: [
    .target(name: "RosettaClient", path: "clients/swift/Sources/RosettaClient"),
    .testTarget(
      name: "RosettaClientTests", dependencies: ["RosettaClient"], path: "clients/swift/Tests/RosettaClientTests"),
  ]
)
