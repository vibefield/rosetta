// The Swift client of the daemon API: the reads a session's reader needs (its catalog row, its timeline pages, an
// exact row, a row's images and oversized text) and its live subscription. Its rules are the TypeScript client's
// (`clients/ts/client.ts`), the one place the subscribe semantics live outside Rust: a protocol version checked on
// every answer and every hello, a 401's reason read from its header, a rewritten session's cursor answered as
// `cursorExpired`, redirects refused, a stored member read only from the daemon's own origin.
//
// The transport is injected: a consumer whose network lives in-process (a tailnet node's netstack) hands in its own
// `URLSession`, and the WebSocket comes from the same session unless a socket factory is given.

import Foundation
import Synchronization

public final class RosettaClient: Sendable {
  public static let defaultBaseURL = URL(string: "http://127.0.0.1:4640")!
  /// The most an image answer may hold, unless the caller names another bound: the daemon's own input ceiling.
  public static let defaultImageBytes = 32 * 1024 * 1024
  /// The most a text answer (a text member, a recovered command) may hold, unless the caller names another bound.
  public static let defaultTextBytes = 4 * 1024 * 1024

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

  /// One deliberately omitted command body, recovered from its raw-ledger record. Nothing is fetched until the
  /// reader opens the disclosure. An answer larger than `maxBytes` throws `RosettaError.tooLarge`.
  public func command(
    _ sessionId: String, timelineId: String, maxBytes: Int = RosettaClient.defaultTextBytes
  ) async throws -> CommandResponse {
    let path = "/api/sessions/\(encodeComponent(sessionId))/timeline/\(encodeComponent(timelineId))/command"
    let asked = try url(path)
    let (data, _) = try await bounded(asked, said: path, maxBytes: maxBytes)
    return try JSONDecoder().decode(CommandResponse.self, from: data)
  }

  /// Subscribe to one session's timeline: a snapshot at a stamp, then its mutations, resumed from the stamp on every
  /// reconnect. The subscription connects at once and lives until `close()`.
  @MainActor
  public func subscribeSession(_ sessionId: String, handlers: SessionSubscriptionHandlers) -> SessionSubscription {
    SessionSubscription(client: self, sessionId: sessionId, handlers: handlers)
  }

  // MARK: Stored members

  /// Where one rendition of a stored image is read. Recipes are finite, versioned and enforced by the daemon, and
  /// every address is a WebP the daemon encoded: a WebP bundle names its own sized members, and a recorded original
  /// (any image without a WebP bundle) is drawn only through `?image=<variant>&v=1`, `full` included.
  public func imageAssetURL(_ asset: AssetRef, variant: ImageVariant) throws -> URL {
    let variants = asset.imageVariants ?? [:]
    let bundled = asset.mediaType == "image/webp" && !variants.isEmpty
    var member: AssetRef?
    if bundled { member = variant == .full ? asset : variants[variant.rawValue] }
    let rendition: AssetRef? = member?.mediaType == "image/webp" ? member : nil
    let url = try memberURL((rendition ?? asset).url, of: "image")
    if rendition != nil { return url }
    // A recorded original, or a bundle missing this rendition: the versioned route encodes it from the member.
    guard var parts = URLComponents(url: url, resolvingAgainstBaseURL: true) else { throw unsupported("image") }
    var items = (parts.queryItems ?? []).filter { $0.name != "image" && $0.name != "v" }
    items.append(URLQueryItem(name: "image", value: variant.rawValue))
    items.append(URLQueryItem(name: "v", value: "1"))
    parts.queryItems = items
    guard let asked = parts.url else { throw unsupported("image") }
    return asked
  }

  /// One rendition of a stored image, as bytes, never decoded here. The TypeScript client hands a browser the
  /// address; a consumer whose network lives in-process has no loader that can reach it, so this client reads it.
  /// An answer that is not a WebP is refused before its body is read: a recorded original is never handed to a
  /// decoder. An answer larger than `maxBytes` throws `RosettaError.tooLarge`. Cancelling the task cancels the read.
  public func image(
    _ asset: AssetRef, variant: ImageVariant, maxBytes: Int = RosettaClient.defaultImageBytes
  ) async throws -> ImageData {
    let url = try imageAssetURL(asset, variant: variant)
    let (data, http) = try await bounded(url, maxBytes: maxBytes) { contentType in
      mediaEssence(contentType) == "image/webp" ? nil : "an image rendition"
    }
    return ImageData(
      bytes: data, contentType: http.value(forHTTPHeaderField: "content-type") ?? "",
      etag: http.value(forHTTPHeaderField: "etag"))
  }

  /// A stored oversized-text member, read when its disclosure is opened. Only a text member is asked for (plain
  /// text, or a canonical document). One larger than `maxBytes` throws `RosettaError.tooLarge`: by its own count
  /// before any request, and by the answer's before its body is held.
  public func readTextAsset(
    _ asset: AssetRef, maxBytes: Int = RosettaClient.defaultTextBytes
  ) async throws -> String {
    guard textMediaTypes.contains(mediaEssence(asset.mediaType)) else {
      throw RosettaError.request(status: 0, code: .badRequest, message: "the stored member is not text")
    }
    guard asset.byteLen <= Int64(maxBytes) else { throw tooLarge(asset.byteLen, maxBytes, "the stored text") }
    let url = try memberURL(asset.url, of: "text")
    let (data, _) = try await bounded(url, maxBytes: maxBytes) { contentType in
      textMediaTypes.contains(mediaEssence(contentType)) ? nil : "text"
    }
    return String(decoding: data, as: UTF8.self)
  }

  /// A stored member's address, resolved against the base URL. Member addresses are the daemon's own, and may not
  /// send this client and its credential to another origin.
  private func memberURL(_ address: String, of what: String) throws -> URL {
    guard let base = URL(string: baseURL.absoluteString.trimmingSuffix("/") + "/"),
      let url = URL(string: address, relativeTo: base)?.absoluteURL,
      let scheme = url.scheme?.lowercased(), scheme == "http" || scheme == "https",
      origin(of: url) == origin(of: base)
    else { throw unsupported(what) }
    return url
  }

  private func unsupported(_ what: String) -> RosettaError {
    .request(status: 0, code: .badRequest, message: "the stored \(what) has an unsupported address")
  }

  // MARK: The request

  func get<T: Decodable>(_ path: String) async throws -> T {
    let asked = try url(path)
    let (data, answer) = try await session.data(for: request(asked), delegate: NoRedirects.shared)
    _ = try await admit(answer, path) { data }
    return try JSONDecoder().decode(T.self, from: data)
  }

  private func url(_ path: String) throws -> URL {
    guard let url = URL(string: baseURL.absoluteString.trimmingSuffix("/") + path) else {
      throw RosettaError.request(status: 0, code: .badRequest, message: "Not a URL: \(path)")
    }
    return url
  }

  /// A GET with the credential, as every request carries it.
  private func request(_ url: URL) -> URLRequest {
    var request = URLRequest(url: url)
    request.timeoutInterval = requestTimeout.seconds
    if let credential { request.setValue(credential.authorizationHeader, forHTTPHeaderField: "authorization") }
    return request
  }

  /// One GET whose body is held only up to `maxBytes`, under the same request rules as `get`. The answer's headers
  /// are judged first: `unwanted` names what was expected when the content type is not it. Then the declared
  /// length, when the answer states one, and the counted body when it does not. A refusal cancels the transfer.
  private func bounded(
    _ url: URL, said path: String? = nil, maxBytes: Int, unwanted: (String) -> String? = { _ in nil }
  ) async throws -> (Data, HTTPURLResponse) {
    let path = path ?? said(url)
    let (stream, answer) = try await session.bytes(for: request(url), delegate: NoRedirects.shared)
    do {
      let http = try await admit(answer, path) {
        let read = try? await RosettaClient.collect(stream, upTo: RosettaClient.errorBodyBytes)
        return read ?? Data()
      }
      if let expected = unwanted(http.value(forHTTPHeaderField: "content-type") ?? "") {
        throw RosettaError.request(
          status: http.statusCode, code: nil, message: "\(path) did not answer with \(expected)")
      }
      if let stated = http.value(forHTTPHeaderField: "content-length"), let declared = Int64(stated),
        declared > Int64(maxBytes)
      {
        throw tooLarge(declared, maxBytes, path)
      }
      guard let data = try await RosettaClient.collect(stream, upTo: maxBytes) else {
        throw tooLarge(nil, maxBytes, path)
      }
      return (data, http)
    } catch {
      stream.task.cancel()
      throw error
    }
  }

  /// The most of a refusal's body that is read for its code and message.
  private static let errorBodyBytes = 64 * 1024

  /// The body, or nil once it holds more than `limit` bytes.
  private static func collect(_ stream: URLSession.AsyncBytes, upTo limit: Int) async throws -> Data? {
    var data = Data()
    for try await byte in stream {
      if data.count == limit { return nil }
      data.append(byte)
    }
    return data
  }

  private func tooLarge(_ byteLen: Int64?, _ limit: Int, _ what: String) -> RosettaError {
    let size = byteLen.map { "\($0) bytes" } ?? "more than \(limit) bytes"
    return .tooLarge(byteLen: byteLen, limit: limit, message: "\(what) is \(size); the bound is \(limit)")
  }

  /// The request rules every answer is held to before its body is used: the protocol version, a 401's reason, a
  /// refusal's code and message (read from `body`). `path` is how the request is named in an error.
  private func admit(
    _ answer: URLResponse, _ path: String, body: () async -> Data
  ) async throws -> HTTPURLResponse {
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
      let raw = await body()
      let body = try? JSONDecoder().decode(ErrorBody.self, from: raw)
      let code = body?.error?.code
      let message = body?.error?.message ?? "\(http.statusCode) on \(path)"
      if code == .cursorExpired { throw RosettaError.cursorExpired(message: message) }
      throw RosettaError.request(status: http.statusCode, code: code, message: message)
    }
    return http
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

/// The media types `readTextAsset` reads: the daemon's two text members.
private let textMediaTypes: Set<String> = ["text/plain", "application/vnd.rosetta.canonical-document+json"]

/// A media type without its parameters, lowercased: `text/plain; charset=utf-8` is `text/plain`.
func mediaEssence(_ contentType: String) -> String {
  let essence = contentType.split(separator: ";", maxSplits: 1, omittingEmptySubsequences: false).first ?? ""
  return essence.trimmingCharacters(in: .whitespaces).lowercased()
}

/// A URL's origin: its scheme, host and port, the scheme's own port when none is written.
private func origin(of url: URL) -> String {
  let scheme = url.scheme?.lowercased() ?? ""
  let port = url.port ?? (scheme == "https" ? 443 : 80)
  return "\(scheme)://\(url.host()?.lowercased() ?? ""):\(port)"
}

/// How a request is named in an error: its path and query, as the TypeScript client names it.
private func said(_ url: URL) -> String {
  url.path(percentEncoded: true) + (url.query(percentEncoded: true).map { "?\($0)" } ?? "")
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
