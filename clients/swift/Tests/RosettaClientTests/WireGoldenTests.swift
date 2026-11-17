// The hand-written wire types against the Rust crate's own golden: every frame, mutation, page and row it pins
// decodes here, a row comes back whole, and the protocol version is the crate's.

import Foundation
import Testing

@testable import RosettaClient

@Suite("The wire golden reads as the Rust crate wrote it")
struct WireGoldenTests {
  @Test("the protocol version is the Rust crate's PROTOCOL_VERSION")
  func protocolVersion() {
    #expect(golden("protocol_version") == .int(Int64(Rosetta.protocolVersion)))
  }

  @Test("every frame decodes to its own case; another subscription's frames are kept by their tag")
  func frames() throws {
    let frames = try golden("frames").arrayValue!.map { try decode(ServerFrame.self, $0) }
    #expect(frames.count == 10)
    let stamp = Stamp(epoch: 9, seq: 3)
    #expect(frames[0] == .hello(protocolVersion: 30, stamp: stamp))
    guard case .timelineSnapshot(let snapStamp, let entries, _, let cursor, let watermark) = frames[1] else {
      Issue.record("the second frame is the timeline snapshot: \(frames[1])")
      return
    }
    #expect(snapStamp == stamp)
    #expect(entries.map(\.timelineId) == ["t1", "team-report-1"])
    #expect(cursor == nil)
    #expect(watermark == "1.2.3")
    guard case .timelineMetaSnapshot(let metaStamp, let meta) = frames[2] else {
      Issue.record("the third frame is the meta snapshot: \(frames[2])")
      return
    }
    #expect(metaStamp == stamp)
    #expect(meta.artifacts.count == goldenFrame("timeline_meta_snapshot")["timeline_meta"]?["artifacts"]?.arrayValue?.count)
    #expect(frames[3] == .caughtUp(stamp: stamp))
    #expect(frames[4] == .other(frame: "collaboration_snapshot"))
    #expect(frames[5] == .other(frame: "response_runtime"))
    #expect(frames[6] == .other(frame: "meta_snapshot"))
    #expect(frames[7] == .mutation(stamp: stamp, mutation: .sessionSwept(sessionId: "s1")))
    #expect(frames[8] == .resync)
    #expect(frames[9] == .error(ApiError(code: .internal, message: "boom")))
  }

  @Test("every mutation decodes: the five a session acts on by their case, the meta subscription's by their tag")
  func mutations() throws {
    let mutations = try golden("mutations").arrayValue!.map { try decode(Mutation.self, $0) }
    let tags = mutations.map { mutation -> String in
      switch mutation {
      case .message: "message"
      case .toolUpdated: "tool_updated"
      case .usage: "usage"
      case .sessionSwept: "session_swept"
      case .timelineMetaChanged: "timeline_meta_changed"
      case .other(let type, _): "other:\(type)"
      }
    }
    #expect(
      tags == [
        "message", "message", "message", "tool_updated", "usage", "session_swept", "timeline_meta_changed",
        "other:session", "other:project", "other:runtime", "other:artifacts_changed",
      ])
    #expect(mutations.prefix(3).map(\.timelineId) == ["t1", "t-command-output", "team-report-1"])
    let usage = goldenMutations("usage")[0]
    guard case .usage(_, let id, _, let model, let tokens, let cost) = mutations[4] else {
      Issue.record("the fifth mutation is the usage: \(mutations[4])")
      return
    }
    #expect(id == "t1")
    #expect(model == usage["model"]?.stringValue)
    #expect(tokens == usage["tokens"])
    #expect(cost == usage["cost"])
  }

  @Test("a message mutation's row is the row without its tag: every field kept as sent")
  func messageRowIsWhole() throws {
    let sent = goldenMutations("message")[0]
    guard case .message(let row) = try decode(Mutation.self, sent) else {
      Issue.record("the first message mutation reads as a message")
      return
    }
    var expected = sent.objectValue!
    expected["type"] = nil
    #expect(row.fields == expected)
  }

  @Test("the page and the exact row decode, and a row re-encodes to the object the daemon sent")
  func pageAndEntry() throws {
    let page = try decode(TimelinePage.self, golden("timeline_page"))
    #expect(page.watermark == "1.2.3")
    #expect(page.cursor == golden("timeline_page")["cursor"]?.stringValue)
    #expect(page.entries.map(\.timelineId) == ["t1", "team-report-1"])
    #expect(page.entries[0].body["type"] == .string("tool_use"))
    let exact = try decode(TimelineEntryResponse.self, golden("timeline_entry"))
    #expect(exact.watermark == "1.2.3")
    #expect(exact.entry.timelineId == "t1")
    let reencoded = try JSONDecoder().decode(JSONValue.self, from: JSONEncoder().encode(exact.entry))
    #expect(reencoded == golden("timeline_entry")["entry"])
  }

  @Test("every stored member the golden pins reads as an AssetRef, a WebP bundle with its sized members")
  func storedMembers() throws {
    /// Every `{"type": "asset", "asset": …}` source anywhere in the golden.
    func members(_ value: JSONValue) -> [JSONValue] {
      switch value {
      case .object(let fields):
        let own = fields["type"] == .string("asset") ? [fields["asset"]].compactMap { $0 } : []
        return own + fields.values.flatMap(members)
      case .array(let items): return items.flatMap(members)
      default: return []
      }
    }
    let pinned = members(wireGolden)
    let bundles = try pinned.map { json in
      let asset = try #require(AssetRef(json), "\(text(json))")
      #expect(try decode(AssetRef.self, json) == asset)
      #expect(asset.url == json["url"]?.stringValue)
      return asset
    }.filter { $0.imageVariants != nil }
    #expect(!bundles.isEmpty, "the golden pins an image with its renditions")
    let names = Set(ImageVariant.allCases.map(\.rawValue))
    #expect(names == ["full", "rail", "tile", "tile2x", "preview"])
    for bundle in bundles {
      #expect(bundle.mediaType == "image/webp")
      for (variant, member) in bundle.imageVariants ?? [:] {
        #expect(names.contains(variant), "\(variant)")
        #expect(member.mediaType == "image/webp")
        #expect(member.url == "/api/assets/\(member.digest).webp")
        #expect(member.imageVariants == nil, "a sized member holds no renditions of its own")
      }
    }
  }

  @Test("the recovered command decodes")
  func recoveredCommand() throws {
    let recovered = try decode(CommandResponse.self, golden("command"))
    #expect(recovered.command == golden("command")["command"]?.stringValue)
    #expect(!recovered.command.isEmpty)
  }

  @Test("every error code the golden names is one this client names")
  func errorCodes() throws {
    for value in golden("error_codes").arrayValue! {
      let code = try decode(ErrorCode.self, value)
      #expect(ErrorCode.known.contains(code), "\(code.rawValue)")
    }
    // and a code a newer daemon adds is kept as sent
    #expect(try decode(ErrorCode.self, .string("a_new_code")).rawValue == "a_new_code")
  }

  @Test("a row is strict on its identity: one without a timeline_id is refused")
  func rowIdentityIsStrict() {
    var fields = golden("timeline_entry")["entry"]!.objectValue!
    fields["timeline_id"] = nil
    #expect(throws: DecodingError.self) { try decode(TimelineEntry.self, .object(fields)) }
  }

  @Test("a work index list a daemon omits reads empty, as the TypeScript client's normalizeTimelineMeta")
  func timelineMetaOmittedList() throws {
    let meta = try decode(TimelineMeta.self, .object(["tasks": .array([]), "plans": .array([])]))
    #expect(meta == .empty)
  }
}
