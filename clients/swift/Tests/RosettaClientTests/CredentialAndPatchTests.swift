// The credential's two presentations, and the canonical patch.

import Foundation
import Testing

@testable import RosettaClient

@Suite("A credential is presented as the TypeScript client presents it")
struct CredentialTests {
  @Test("the Authorization header: Bearer, or Rosetta-Grant")
  func authorization() {
    #expect(RosettaCredential.bearer("tok").authorizationHeader == "Bearer tok")
    #expect(RosettaCredential.grant("g.1").authorizationHeader == "Rosetta-Grant g.1")
  }

  @Test("the WebSocket pair: the marker the daemon selects, then the credential; a bearer as unpadded base64url")
  func webSocketProtocols() {
    #expect(RosettaCredential.bearer("tok").webSocketProtocols == ["rosetta", "rosetta.bearer.dG9r"])
    #expect(RosettaCredential.grant("g.1").webSocketProtocols == ["rosetta", "rosetta.grant.g.1"])
    // base64url's two letters and no padding: 0xfb 0xff is "+/8=" in base64
    #expect(base64URL(Data([0xfb, 0xff])) == "-_8")
  }

  @Test("a 401's reason is one of the nine the daemon names")
  func refusals() {
    #expect(AuthRefusal.allCases.map(\.rawValue) == [
      "missing", "bad-token", "malformed", "bad-signature", "expired", "wrong-session", "wrong-scope", "wrong-route",
      "grants-disabled",
    ])
  }
}

@Suite("The canonical patch, as the TypeScript client's patchRow")
struct PatchTests {
  private func toolRow(_ tool: JSONValue) throws -> TimelineEntry {
    try TimelineEntry(fields: [
      "timeline_id": .string("t1"), "session_id": .string("s1"), "ordinal": .int(1),
      "body": .object(["type": .string("tool_use"), "tool": tool]),
    ])
  }

  private func workflow(run: JSONValue, runId: String) -> JSONValue {
    .object(["action": .object(["type": .string("workflow"), "run": run, "run_id": .string(runId)])])
  }

  @Test("a tool update keeps the hydrated workflow run while both actions name the same run, and only then")
  func toolUpdateKeepsTheRun() throws {
    let run = JSONValue.object(["status": .string("running")])
    let row = try toolRow(workflow(run: run, runId: "r1"))
    let same = patchRow(row, .toolUpdated(sessionId: "s1", timelineId: "t1", tool: workflow(run: .null, runId: "r1"), at: 5))
    #expect(same?.body["tool"]?["action"]?["run"] == run)
    let other = patchRow(row, .toolUpdated(sessionId: "s1", timelineId: "t1", tool: workflow(run: .null, runId: "r2"), at: 5))
    #expect(other?.body["tool"]?["action"]?["run"] == .null)
  }

  @Test("a usage patch replaces the usage, model and time, and never keeps the old cost")
  func usageReplacesTheCost() throws {
    let row = try toolRow(.object([:])).with("cost", .object(["micro_usd": .int(9)]))
    let tokens = JSONValue.object(["input": .int(10)])
    let priced = patchRow(
      row, .usage(sessionId: "s1", timelineId: "t1", ts: 7, model: "m", tokens: tokens, cost: .object(["micro_usd": .int(1)])))
    #expect(priced?["usage"] == tokens)
    #expect(priced?["model"] == .string("m"))
    #expect(priced?["at"] == .int(7))
    #expect(priced?["cost"] == .object(["micro_usd": .int(1)]))
    let unpriced = patchRow(row, .usage(sessionId: "s1", timelineId: "t1", ts: nil, model: nil, tokens: tokens, cost: nil))
    #expect(unpriced?["cost"] == nil)
    #expect(unpriced?["model"] == .null)
  }

  @Test("a patch to a row this client does not hold holds nothing; a message is a whole row")
  func patchesNeedARow() throws {
    #expect(patchRow(nil, .toolUpdated(sessionId: "s1", timelineId: "t1", tool: .null, at: nil)) == nil)
    let row = try toolRow(.object([:]))
    #expect(patchRow(nil, .message(row)) == row)
  }
}
