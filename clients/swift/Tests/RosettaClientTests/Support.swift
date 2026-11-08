// The tests' instruments: the Rust crate's wire golden (`crates/protocol/tests/golden/wire.json`, the serialized JSON
// of every wire type, pinned on the Rust side), a scripted socket, an HTTP stub and a wait.

import Foundation
import Synchronization
import Testing

@testable import RosettaClient

// MARK: The wire golden

/// The golden, read where the Rust test writes it: this file is clients/swift/Tests/RosettaClientTests/Support.swift.
let wireGolden: JSONValue = {
  var root = URL(fileURLWithPath: #filePath)
  for _ in 0..<5 { root.deleteLastPathComponent() }
  let url = root.appendingPathComponent("crates/protocol/tests/golden/wire.json")
  do {
    return try JSONDecoder().decode(JSONValue.self, from: Data(contentsOf: url))
  } catch {
    fatalError("the wire golden at \(url.path) does not read: \(error)")
  }
}()

func golden(_ key: String) -> JSONValue {
  guard let value = wireGolden[key] else { fatalError("the wire golden has no \(key)") }
  return value
}

/// The golden's frame tagged `frame`.
func goldenFrame(_ frame: String) -> JSONValue {
  guard let found = golden("frames").arrayValue?.first(where: { $0["frame"] == .string(frame) }) else {
    fatalError("the wire golden has no \(frame) frame")
  }
  return found
}

/// The golden's mutations tagged `type`, in order.
func goldenMutations(_ type: String) -> [JSONValue] {
  golden("mutations").arrayValue?.filter { $0["type"] == .string(type) } ?? []
}

func text(_ value: JSONValue) -> String {
  String(decoding: try! JSONEncoder().encode(value), as: UTF8.self)
}

func decode<T: Decodable>(_ type: T.Type, _ value: JSONValue) throws -> T {
  try JSONDecoder().decode(type, from: JSONEncoder().encode(value))
}

/// A mutation frame carrying `mutation` at `stamp`.
func mutationFrame(_ mutation: JSONValue, epoch: Int64 = 9, seq: Int64) -> String {
  text(
    .object([
      "frame": .string("mutation"), "stamp": .object(["epoch": .int(epoch), "seq": .int(seq)]), "mutation": mutation,
    ]))
}

// MARK: A scripted socket

final class ScriptedSocket: RosettaSocket {
  private struct State {
    var queue: [String] = []
    var waiter: CheckedContinuation<String, any Error>?
    var closed = false
  }
  private let state = Mutex(State())

  func receive() async throws -> String {
    try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<String, any Error>) in
      let ready: Result<String, any Error>? = state.withLock { state in
        if !state.queue.isEmpty { return .success(state.queue.removeFirst()) }
        if state.closed { return .failure(CancellationError()) }
        state.waiter = continuation
        return nil
      }
      if let ready { continuation.resume(with: ready) }
    }
  }

  /// The daemon sends `frame`.
  func deliver(_ frame: String) {
    let waiter: CheckedContinuation<String, any Error>? = state.withLock { state in
      if let waiter = state.waiter {
        state.waiter = nil
        return waiter
      }
      state.queue.append(frame)
      return nil
    }
    waiter?.resume(returning: frame)
  }

  func close() {
    let waiter = state.withLock { state in
      state.closed = true
      let waiter = state.waiter
      state.waiter = nil
      return waiter
    }
    waiter?.resume(throwing: CancellationError())
  }

  var isClosed: Bool { state.withLock { $0.closed } }
}

final class ScriptedSockets: RosettaSocketFactory {
  struct Dial: Sendable {
    let url: URL
    let protocols: [String]
    let socket: ScriptedSocket
  }
  private let opened = Mutex<[Dial]>([])

  func open(url: URL, protocols: [String]) -> any RosettaSocket {
    let socket = ScriptedSocket()
    opened.withLock { $0.append(Dial(url: url, protocols: protocols, socket: socket)) }
    return socket
  }

  var dials: [Dial] { opened.withLock { $0 } }
  var latest: ScriptedSocket? { dials.last?.socket }
}

// MARK: An HTTP stub

/// Answers every request of a session built by `StubHTTP.session()`. One stub at a time: the suites that use it are
/// serialized.
final class StubHTTP: URLProtocol, @unchecked Sendable {
  struct Answer: Sendable {
    var status: Int
    var headers: [String: String]
    var body: Data
  }
  nonisolated(unsafe) static var answer: @Sendable (URLRequest) -> Answer = { _ in Answer(status: 500, headers: [:], body: Data()) }
  static let requests = Mutex<[URLRequest]>([])

  static func session() -> URLSession {
    requests.withLock { $0.removeAll() }
    let configuration = URLSessionConfiguration.ephemeral
    configuration.protocolClasses = [StubHTTP.self]
    return URLSession(configuration: configuration)
  }

  /// An answer with this protocol's version header.
  static func json(_ status: Int, _ body: JSONValue, headers: [String: String] = [:]) -> Answer {
    var all = ["content-type": "application/json", Rosetta.protocolVersionHeader: String(Rosetta.protocolVersion)]
    all.merge(headers) { _, given in given }
    return Answer(status: status, headers: all, body: try! JSONEncoder().encode(body))
  }

  override class func canInit(with request: URLRequest) -> Bool { true }
  override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

  override func startLoading() {
    Self.requests.withLock { $0.append(request) }
    let answer = Self.answer(request)
    let response = HTTPURLResponse(
      url: request.url!, statusCode: answer.status, httpVersion: "HTTP/1.1", headerFields: answer.headers)!
    client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
    client?.urlProtocol(self, didLoad: answer.body)
    client?.urlProtocolDidFinishLoading(self)
  }

  override func stopLoading() {}
}

// MARK: Waiting

struct WaitTimeout: Error, CustomStringConvertible {
  let description: String
}

@MainActor
func waitUntil(_ what: String, timeout: Duration = .seconds(5), _ condition: @MainActor () -> Bool) async throws {
  let clock = ContinuousClock()
  let deadline = clock.now + timeout
  while !condition() {
    if clock.now >= deadline { throw WaitTimeout(description: "not within \(timeout): \(what)") }
    try await Task.sleep(for: .milliseconds(5))
  }
}
