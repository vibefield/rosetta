// The Swift client of the daemon API: the reads a session's reader needs (its catalog row, its timeline pages, an
// exact row) and its live subscription. Its rules are the TypeScript client's (`clients/ts/client.ts`), the one place
// the subscribe semantics live outside Rust: a protocol version checked on every answer and every hello, a 401's
// reason read from its header, a rewritten session's cursor answered as `cursorExpired`, redirects refused.
//
// The transport is injected: a consumer whose network lives in-process (a tailnet node's netstack) hands in its own
// `URLSession`, and the WebSocket comes from the same session unless a socket factory is given.

import Foundation
import Synchronization

public final class RosettaClient: Sendable {
  public static let defaultBaseURL = URL(string: "http://127.0.0.1:4640")!

  public let baseURL: URL
  let session: URLSession
  let sockets: any RosettaSocketFactory
  let requestTimeout: Duration
  let notificationInterval: Duration
  private let credentialBox: Mutex<RosettaCredential?>

  /// - Parameters:
  ///   - session: every request and, unless `sockets` is given, every WebSocket rides it.
  ///   - notificationInterval: how long a subscription coalesces changes before it says them; zero says each at once.
  public init(
    baseURL: URL = RosettaClient.defaultBaseURL, session: URLSession = .shared, credential: RosettaCredential? = nil,
    sockets: (any RosettaSocketFactory)? = nil, requestTimeout: Duration = .seconds(30),
    notificationInterval: Duration = .milliseconds(16)
  ) {
    self.baseURL = baseURL
    self.session = session
    self.sockets = sockets ?? URLSessionSocketFactory(session: session)
    self.requestTimeout = requestTimeout
    self.notificationInterval = notificationInterval
    self.credentialBox = Mutex(credential)
  }

  /// The credential later requests and every later (re)connect present.
  public var credential: RosettaCredential? { credentialBox.withLock { $0 } }

  /// Replace the credential (a refreshed grant, a rotated bearer), or drop it. An open socket is left alone: the
  /// daemon checks a credential at the upgrade, so a grant that expires under a live subscription ends only with its
  /// socket, and the reconnect then needs a fresh one.
  public func setCredential(_ credential: RosettaCredential?) {
    credentialBox.withLock { $0 = credential }
  }

  // MARK: Reads

  public func session(_ id: String) async throws -> SessionResponse {
    try await get("/api/sessions/\(encodeComponent(id))")
  }

  /// One backward page. Pass the previous page's `cursor` to continue; a rewrite underneath throws
  /// `RosettaError.cursorExpired`: refetch from the head (no cursor).
  public func timeline(_ sessionId: String, limit: Int? = nil, cursor: String? = nil) async throws -> TimelinePage {
    var query: [(String, String)] = []
    if let limit { query.append(("limit", String(limit))) }
    if let cursor { query.append(("cursor", cursor)) }
    return try await get("/api/sessions/\(encodeComponent(sessionId))/timeline\(queryString(query))")
  }

  /// An exact row and the generation it belongs to.
  public func timelineEntry(_ sessionId: String, timelineId: String) async throws -> TimelineEntryResponse {
    try await get("/api/sessions/\(encodeComponent(sessionId))/timeline/\(encodeComponent(timelineId))")
  }

  /// Subscribe to one session's timeline: a snapshot at a stamp, then its mutations, resumed from the stamp on every
  /// reconnect. The subscription connects at once and lives until `close()`.
  @MainActor
  public func subscribeSession(_ sessionId: String, handlers: SessionSubscriptionHandlers) -> SessionSubscription {
    SessionSubscription(client: self, sessionId: sessionId, handlers: handlers)
  }

  // MARK: The request

  func get<T: Decodable>(_ path: String) async throws -> T {
    let data = try await response(path)
    return try JSONDecoder().decode(T.self, from: data)
  }

  private func response(_ path: String) async throws -> Data {
    guard let url = URL(string: baseURL.absoluteString.trimmingSuffix("/") + path) else {
      throw RosettaError.request(status: 0, code: .badRequest, message: "Not a URL: \(path)")
    }
    var request = URLRequest(url: url)
    request.timeoutInterval = requestTimeout.seconds
    if let credential { request.setValue(credential.authorizationHeader, forHTTPHeaderField: "authorization") }
    let (data, answer) = try await session.data(for: request, delegate: NoRedirects.shared)
    guard let http = answer as? HTTPURLResponse else {
      throw RosettaError.request(status: 0, code: nil, message: "No HTTP answer on \(path)")
    }
    let version = http.value(forHTTPHeaderField: Rosetta.protocolVersionHeader)
    guard version == String(Rosetta.protocolVersion) else {
      throw RosettaError.request(
        status: http.statusCode, code: .incompatibleProtocol,
        message: "Daemon protocol \(version ?? "unknown") is incompatible with this app (expected "
          + "\(Rosetta.protocolVersion)). Restart with the matching daemon.")
    }
    if http.statusCode == 401 {
      let reason = http.value(forHTTPHeaderField: Rosetta.authReasonHeader)
      throw RosettaError.unauthorized(
        reason: reason.flatMap(AuthRefusal.init(rawValue:)),
        message: "The daemon refused \(path) (\(reason ?? "unauthorized")).")
    }
    guard (200..<300).contains(http.statusCode) else {
      let body = try? JSONDecoder().decode(ErrorBody.self, from: data)
      let code = body?.error?.code
      let message = body?.error?.message ?? "\(http.statusCode) on \(path)"
      if code == .cursorExpired { throw RosettaError.cursorExpired(message: message) }
      throw RosettaError.request(status: http.statusCode, code: code, message: message)
    }
    return data
  }

  /// The WebSocket URL of `path` (the base URL's scheme, http → ws).
  func socketURL(_ path: String) -> String {
    let base = baseURL.absoluteString.trimmingSuffix("/")
    let socketBase = base.hasPrefix("http") ? "ws" + String(base.dropFirst(4)) : base
    return socketBase + path
  }
}

private struct ErrorBody: Decodable {
  let error: ApiError?
}

/// The TypeScript client's `redirect: "error"`: a redirect is answered as the 3xx it is, never followed.
private final class NoRedirects: NSObject, URLSessionTaskDelegate, Sendable {
  static let shared = NoRedirects()

  func urlSession(
    _ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse,
    newRequest request: URLRequest
  ) async -> URLRequest? {
    nil
  }
}

/// `encodeURIComponent`'s set: ASCII letters and digits, and `-_.!~*'()`.
func encodeComponent(_ text: String) -> String {
  text.addingPercentEncoding(withAllowedCharacters: componentAllowed) ?? text
}

private let componentAllowed = CharacterSet(
  charactersIn: "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_.!~*'()")

func queryString(_ pairs: [(String, String)]) -> String {
  pairs.isEmpty ? "" : "?" + pairs.map { "\(encodeComponent($0.0))=\(encodeComponent($0.1))" }.joined(separator: "&")
}

extension String {
  func trimmingSuffix(_ suffix: String) -> String {
    hasSuffix(suffix) ? String(dropLast(suffix.count)) : self
  }
}

extension Duration {
  /// The duration in seconds, as Foundation's intervals count it.
  var seconds: TimeInterval {
    let (whole, attoseconds) = components
    return TimeInterval(whole) + TimeInterval(attoseconds) / 1e18
  }
}
