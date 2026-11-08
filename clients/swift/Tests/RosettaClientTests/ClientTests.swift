// The reads, against an HTTP stub: the TypeScript client's request rules (`RosettaClient.response`).

import Foundation
import Testing

@testable import RosettaClient

@Suite("The reads keep the TypeScript client's request rules", .serialized)
struct ClientTests {
  private func client(_ credential: RosettaCredential? = .grant("g1")) -> RosettaClient {
    RosettaClient(
      baseURL: URL(string: "http://127.0.0.1:4640")!, session: StubHTTP.session(), credential: credential,
      sockets: ScriptedSockets())
  }

  /// The error `ask` throws, when it is the client's.
  private func failure(_ ask: () async throws -> Void) async -> RosettaError? {
    do {
      try await ask()
      Issue.record("the request succeeded")
      return nil
    } catch let error as RosettaError {
      return error
    } catch {
      Issue.record("not the client's error: \(error)")
      return nil
    }
  }

  @Test("a page is asked at its encoded path and query, with the credential, and read whole")
  func timelinePage() async throws {
    StubHTTP.answer = { _ in StubHTTP.json(200, golden("timeline_page")) }
    let page = try await client().timeline("s/1", limit: 2, cursor: "c 1")
    #expect(page.entries.map(\.timelineId) == ["t1", "team-report-1"])
    let asked = try #require(StubHTTP.requests.withLock { $0.last })
    #expect(asked.url?.absoluteString == "http://127.0.0.1:4640/api/sessions/s%2F1/timeline?limit=2&cursor=c%201")
    #expect(asked.value(forHTTPHeaderField: "authorization") == "Rosetta-Grant g1")
  }

  @Test("an exact row is asked by both ids; no credential sends no Authorization")
  func exactRow() async throws {
    StubHTTP.answer = { _ in StubHTTP.json(200, golden("timeline_entry")) }
    let exact = try await client(nil).timelineEntry("s1", timelineId: "t1")
    #expect(exact.entry.timelineId == "t1")
    let asked = try #require(StubHTTP.requests.withLock { $0.last })
    #expect(asked.url?.path == "/api/sessions/s1/timeline/t1")
    #expect(asked.value(forHTTPHeaderField: "authorization") == nil)
  }

  @Test("an answer without this protocol's version is incompatible_protocol, whatever its status")
  func versionHeader() async {
    StubHTTP.answer = { _ in StubHTTP.Answer(status: 200, headers: [:], body: Data("{}".utf8)) }
    let error = await failure { _ = try await client().session("s1") }
    #expect(error?.code == .incompatibleProtocol)
    #expect(error?.message.contains("expected 30") == true)
  }

  @Test("a 401 names its reason from x-rosetta-auth")
  func unauthorized() async {
    StubHTTP.answer = { _ in StubHTTP.json(401, .object([:]), headers: ["x-rosetta-auth": "expired"]) }
    let error = await failure { _ = try await client().session("s1") }
    #expect(error == .unauthorized(reason: .expired, message: "The daemon refused /api/sessions/s1 (expired)."))
  }

  @Test("a 410 cursor_expired is its own error; any other refusal keeps its status, code and message")
  func refusals() async {
    StubHTTP.answer = { _ in
      StubHTTP.json(410, .object(["error": .object(["code": .string("cursor_expired"), "message": .string("rewritten")])]))
    }
    #expect(await failure { _ = try await client().timeline("s1", cursor: "c") } == .cursorExpired(message: "rewritten"))
    StubHTTP.answer = { _ in
      StubHTTP.json(404, .object(["error": .object(["code": .string("not_found"), "message": .string("no such session")])]))
    }
    #expect(
      await failure { _ = try await client().session("s9") }
        == .request(status: 404, code: .notFound, message: "no such session"))
  }

  @Test("a page fetched under a subscription is held until revealed; a rewritten one restarts the subscription")
  @MainActor
  func readsUnderASubscription() async throws {
    let sockets = ScriptedSockets()
    let (_, sub, recorder) = subscribed(sockets, session: StubHTTP.session())
    defer { sub.close() }
    let socket = try #require(sockets.latest)
    try await greetAndSnapshot(socket, sub)

    var older = golden("timeline_page")["entries"]!.arrayValue![0].objectValue!
    older["timeline_id"] = .string("t0")
    older["ordinal"] = .int(6)
    let held = JSONValue.object([
      "entries": .array([.object(older)]), "watermark": .string("1.2.3"), "cursor": .null,
    ])
    StubHTTP.answer = { _ in StubHTTP.json(200, held) }
    let page = try await sub.fetchPage(cursor: "c1", limit: 5)
    #expect(page.entries.map(\.timelineId) == ["t0"])
    #expect(sub.entries.map(\.timelineId) == ["t1", "team-report-1"], "held, not shown")
    sub.reveal(page.entries)
    #expect(sub.entries.map(\.timelineId) == ["t0", "t1", "team-report-1"])

    let rewritten = JSONValue.object(["entries": .array([]), "watermark": .string("9.9.9"), "cursor": .null])
    StubHTTP.answer = { _ in StubHTTP.json(200, rewritten) }
    await #expect(throws: RosettaError.cursorExpired(message: "The session was rewritten")) {
      try await sub.fetchPage(cursor: "c2")
    }
    #expect(recorder.errors.last == ApiError(code: .cursorExpired, message: "The session was rewritten"))
    #expect(sockets.dials.count == 2, "the subscription restarted")
    #expect(sockets.dials[1].url.query == nil)
  }
}
