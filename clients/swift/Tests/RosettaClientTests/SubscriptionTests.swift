// The subscribe state machine, against a scripted socket and the golden's own frames: the TypeScript client's
// semantics (`clients/ts/client.ts`), row by row.

import Foundation
import Testing

@testable import RosettaClient

/// What a subscription said, in order.
@MainActor
final class Recorder {
  var changes: [[String]] = []
  var statuses: [SubscriptionStatus] = []
  var errors: [ApiError] = []
  var snapshots: [TimelinePage] = []
  var resets = 0

  var handlers: SessionSubscriptionHandlers {
    SessionSubscriptionHandlers(
      onReset: { [self] in resets += 1 },
      onSnapshot: { [self] in snapshots.append($0) },
      onChange: { [self] in changes.append($0.map(\.timelineId)) },
      onStatus: { [self] in statuses.append($0) },
      onError: { [self] in errors.append($0) })
  }
}

@MainActor
func subscribed(
  _ sockets: ScriptedSockets, session: URLSession = .shared
) -> (RosettaClient, SessionSubscription, Recorder) {
  let client = RosettaClient(
    baseURL: URL(string: "http://127.0.0.1:4640")!, session: session, credential: .grant("g1"), sockets: sockets,
    notificationInterval: .zero)
  let recorder = Recorder()
  return (client, client.subscribeSession("s1", handlers: recorder.handlers), recorder)
}

/// Hello, then the golden snapshot (stamp 9/3, rows t1 and team-report-1), on `socket`.
@MainActor
func greetAndSnapshot(_ socket: ScriptedSocket, _ sub: SessionSubscription) async throws {
  socket.deliver(text(goldenFrame("hello")))
  socket.deliver(text(goldenFrame("timeline_snapshot")))
  try await waitUntil("live after the snapshot") { sub.status == .live }
}

@MainActor
@Suite("A session subscription keeps the TypeScript client's protocol")
struct SubscriptionTests {
  @Test("it dials the session's socket with the credential's pair, hears hello, and is live at the snapshot")
  func snapshotThenLive() async throws {
    let sockets = ScriptedSockets()
    let (_, sub, recorder) = subscribed(sockets)
    defer { sub.close() }
    let dial = try #require(sockets.dials.first)
    #expect(dial.url.absoluteString == "ws://127.0.0.1:4640/ws/sessions/s1")
    #expect(dial.protocols == ["rosetta", "rosetta.grant.g1"])
    try await greetAndSnapshot(dial.socket, sub)
    #expect(recorder.statuses == [.connecting, .syncing, .live])
    #expect(recorder.snapshots.map { $0.entries.map(\.timelineId) } == [["t1", "team-report-1"]])
    #expect(recorder.snapshots.first?.watermark == "1.2.3")
    #expect(recorder.changes.last == ["t1", "team-report-1"])
    #expect(recorder.resets == 0, "the first snapshot replaces nothing")
  }

  @Test("a message adds its row in ordinal order; usage and a tool update patch the row they name")
  func mutationsApply() async throws {
    let sockets = ScriptedSockets()
    let (_, sub, recorder) = subscribed(sockets)
    defer { sub.close() }
    let socket = try #require(sockets.latest)
    try await greetAndSnapshot(socket, sub)

    socket.deliver(mutationFrame(goldenMutations("message")[1], seq: 4))
    try await waitUntil("the new row") { sub.entries.count == 3 }
    #expect(sub.entries.map(\.timelineId) == ["t1", "t-command-output", "team-report-1"])

    let usage = goldenMutations("usage")[0]
    socket.deliver(mutationFrame(usage, seq: 5))
    try await waitUntil("the usage patch") { sub.entries.first?["cost"] == usage["cost"] }
    #expect(sub.entries.first?["usage"] == usage["tokens"])
    #expect(sub.entries.first?["model"] == usage["model"])

    let update = goldenMutations("tool_updated")[0]
    socket.deliver(mutationFrame(update, seq: 6))
    try await waitUntil("the tool patch") { sub.entries.first?.body["tool"] == update["tool"] }
    #expect(recorder.errors.isEmpty)
    #expect(sub.status == .live)
  }

  @Test("a dropped socket redials on the backoff, resuming from the stamp; a resync then drops the generation")
  func reconnectResumesThenResync() async throws {
    let sockets = ScriptedSockets()
    let (_, sub, recorder) = subscribed(sockets)
    defer { sub.close() }
    let first = try #require(sockets.latest)
    try await greetAndSnapshot(first, sub)

    first.close()  // the daemon goes away
    try await waitUntil("the redial", timeout: .seconds(3)) { sockets.dials.count == 2 }
    let second = sockets.dials[1]
    #expect(second.url.query == "epoch=9&seq=3")
    #expect(recorder.statuses.suffix(2) == [.live, .connecting])

    second.socket.deliver(text(goldenFrame("hello")))
    second.socket.deliver(text(goldenFrame("resync")))
    try await waitUntil("resyncing") { sub.status == .resyncing }
    #expect(recorder.resets == 1)
    #expect(sub.entries.isEmpty)
    second.socket.deliver(text(goldenFrame("timeline_snapshot")))
    try await waitUntil("live again") { sub.status == .live }
    #expect(sub.entries.map(\.timelineId) == ["t1", "team-report-1"])
  }

  @Test("session_swept restarts at once with no stamp: a fresh snapshot, never a resume")
  func sweptRestarts() async throws {
    let sockets = ScriptedSockets()
    let (_, sub, recorder) = subscribed(sockets)
    defer { sub.close() }
    let first = try #require(sockets.latest)
    try await greetAndSnapshot(first, sub)

    first.deliver(text(goldenFrame("mutation")))  // the golden's mutation frame is the sweep
    try await waitUntil("the restart's dial") { sockets.dials.count == 2 }
    #expect(sockets.dials[1].url.query == nil)
    #expect(first.isClosed)
    #expect(recorder.resets == 1)
    #expect(sub.entries.isEmpty)
  }

  @Test("a hello of another protocol closes the subscription, said as incompatible_protocol, and nothing redials")
  func incompatibleHello() async throws {
    let sockets = ScriptedSockets()
    let (_, sub, recorder) = subscribed(sockets)
    let socket = try #require(sockets.latest)
    var hello = goldenFrame("hello").objectValue!
    hello["protocol_version"] = .int(29)
    socket.deliver(text(.object(hello)))
    try await waitUntil("closed") { sub.status == .closed }
    #expect(recorder.errors.map(\.code) == [.incompatibleProtocol])
    #expect(socket.isClosed)
    try await Task.sleep(for: .milliseconds(700))
    #expect(sockets.dials.count == 1)
  }

  @Test("data before hello is the daemon's fault: said as internal, the socket dropped and redialled")
  func dataBeforeHello() async throws {
    let sockets = ScriptedSockets()
    let (_, sub, recorder) = subscribed(sockets)
    defer { sub.close() }
    let socket = try #require(sockets.latest)
    socket.deliver(text(goldenFrame("timeline_snapshot")))
    try await waitUntil("the error") { !recorder.errors.isEmpty }
    #expect(recorder.errors == [ApiError(code: .internal, message: "The daemon sent data before its protocol handshake.")])
    #expect(socket.isClosed)
    try await waitUntil("the redial", timeout: .seconds(3)) { sockets.dials.count == 2 }
    #expect(recorder.snapshots.isEmpty)
  }

  @Test("close() ends it: closed said once, the socket closed, no redial, and a read refused")
  func closeEndsIt() async throws {
    let sockets = ScriptedSockets()
    let (_, sub, recorder) = subscribed(sockets)
    let socket = try #require(sockets.latest)
    try await greetAndSnapshot(socket, sub)
    sub.close()
    sub.close()
    #expect(recorder.statuses.filter { $0 == .closed }.count == 1)
    #expect(socket.isClosed)
    try await Task.sleep(for: .milliseconds(700))
    #expect(sockets.dials.count == 1)
    await #expect(throws: RosettaError.self) { try await sub.fetchPage(cursor: "c") }
  }
}
