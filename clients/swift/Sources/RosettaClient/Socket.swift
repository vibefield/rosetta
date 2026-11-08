// The WebSocket a subscription reads, behind a seam: by default a `URLSessionWebSocketTask` from the client's own
// session (an in-process tailnet's session carries its sockets too); in tests, a scripted one.

import Foundation

public protocol RosettaSocket: Sendable {
  /// The next text frame. Throws once the socket is closed, by either side, or failed to open.
  func receive() async throws -> String
  /// Close the socket; a pending `receive()` then throws.
  func close()
}

public protocol RosettaSocketFactory: Sendable {
  /// A socket opening to `url`, offering `protocols` (empty: no subprotocol).
  func open(url: URL, protocols: [String]) -> any RosettaSocket
}

public struct URLSessionSocketFactory: RosettaSocketFactory {
  public let session: URLSession
  /// URLSession's default is 1 MiB, which a long session's snapshot passes. A browser's WebSocket (the TypeScript
  /// client's) has no such ceiling.
  public var maximumMessageSize: Int

  public init(session: URLSession, maximumMessageSize: Int = 256 << 20) {
    self.session = session
    self.maximumMessageSize = maximumMessageSize
  }

  public func open(url: URL, protocols: [String]) -> any RosettaSocket {
    let task =
      protocols.isEmpty
      ? session.webSocketTask(with: url) : session.webSocketTask(with: url, protocols: protocols)
    task.maximumMessageSize = maximumMessageSize
    task.resume()
    return URLSessionSocket(task: task)
  }
}

private final class URLSessionSocket: RosettaSocket {
  let task: URLSessionWebSocketTask

  init(task: URLSessionWebSocketTask) { self.task = task }

  func receive() async throws -> String {
    switch try await task.receive() {
    case .string(let text): return text
    case .data(let data): return String(decoding: data, as: UTF8.self)
    @unknown default: throw URLError(.cannotParseResponse)
    }
  }

  func close() {
    task.cancel(with: .normalClosure, reason: nil)
  }
}
