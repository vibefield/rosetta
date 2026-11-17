// How a client proves itself to a daemon started with `--token-file` (docs/AUTH.md), in the TypeScript client's
// words (`clients/ts/client.ts`): a bearer opens every route; a grant opens one session's recorded history until its
// `exp`. This client only carries a grant, as an opaque string: minting needs the key, and minting is Node's
// (`@vibefield/rosetta-protocol/grants`).

import Foundation

public enum RosettaCredential: Sendable, Hashable {
  case bearer(String)
  case grant(String)

  /// The `Authorization` value that presents the credential.
  public var authorizationHeader: String {
    switch self {
    case .bearer(let token): "Bearer \(token)"
    case .grant(let grant): "Rosetta-Grant \(grant)"
    }
  }

  /// The subprotocol list a credentialed WebSocket upgrade offers: the marker the daemon selects, and the credential
  /// beside it. The daemon never echoes the credential entry.
  public var webSocketProtocols: [String] {
    switch self {
    case .bearer(let token): [Rosetta.authWebSocketProtocol, "rosetta.bearer.\(base64URL(Data(token.utf8)))"]
    case .grant(let grant): [Rosetta.authWebSocketProtocol, "rosetta.grant.\(grant)"]
    }
  }
}

/// Why a daemon refused a request: the `x-rosetta-auth` header of its 401.
public enum AuthRefusal: String, Sendable, CaseIterable {
  case missing
  case badToken = "bad-token"
  case malformed
  case badSignature = "bad-signature"
  case expired
  case wrongSession = "wrong-session"
  case wrongScope = "wrong-scope"
  case wrongRoute = "wrong-route"
  case grantsDisabled = "grants-disabled"
}

/// The constants the wire names, kept beside the TypeScript client's.
public enum Rosetta {
  /// The wire version this client speaks; held to the Rust crate's `PROTOCOL_VERSION` by the wire golden test.
  public static let protocolVersion = 30
  public static let protocolVersionHeader = "x-rosetta-protocol-version"
  public static let authReasonHeader = "x-rosetta-auth"
  /// The subprotocol a daemon selects for a credentialed WebSocket upgrade.
  public static let authWebSocketProtocol = "rosetta"
}

/// Unpadded base64url, as `clients/ts/base64url.ts` writes it.
func base64URL(_ data: Data) -> String {
  data.base64EncodedString()
    .replacingOccurrences(of: "+", with: "-")
    .replacingOccurrences(of: "/", with: "_")
    .replacingOccurrences(of: "=", with: "")
}
