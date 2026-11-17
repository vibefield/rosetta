// The wire shapes this client reads, hand-written over the Rust protocol crate (`crates/protocol`) and held to it by
// the crate's own wire golden (`crates/protocol/tests/golden/wire.json`, read by `WireGoldenTests`). They are
// strict where the subscribe protocol keys (a row's identity and order, a stamp, a frame's and a mutation's tag)
// and whole JSON everywhere else: a row's body, usage, tool and every field a newer daemon adds travel as sent,
// for the consumer to read. No agent data is parsed here (iron rule 6): rows are the daemon's projection.

import Foundation

/// A subscription position: ring identity plus the last mutation sequence covered.
public struct Stamp: Sendable, Hashable, Codable {
  public var epoch: Int64
  public var seq: Int64

  public init(epoch: Int64, seq: Int64) {
    self.epoch = epoch
    self.seq = seq
  }
}

/// One timeline row: its identity and order, read strictly, and the whole row as the daemon sent it.
public struct TimelineEntry: Sendable, Hashable {
  public let timelineId: String
  public let sessionId: String
  public let ordinal: Int64
  /// Every field of the row, `timeline_id`, `session_id` and `ordinal` among them.
  public private(set) var fields: [String: JSONValue]

  public init(fields: [String: JSONValue]) throws {
    guard let timelineId = fields["timeline_id"]?.stringValue,
      let sessionId = fields["session_id"]?.stringValue,
      let ordinal = fields["ordinal"]?.intValue
    else {
      throw DecodingError.dataCorrupted(
        .init(codingPath: [], debugDescription: "a timeline entry names its timeline_id, session_id and ordinal"))
    }
    self.timelineId = timelineId
    self.sessionId = sessionId
    self.ordinal = ordinal
    self.fields = fields
  }

  public subscript(key: String) -> JSONValue? { fields[key] }

  /// The row's body (`TimelineBody`), as sent.
  public var body: JSONValue { fields["body"] ?? .null }

  /// The row with `key` replaced; identity keys are not replaceable.
  func with(_ key: String, _ value: JSONValue?) -> TimelineEntry {
    precondition(key != "timeline_id" && key != "session_id" && key != "ordinal", "a row's identity is fixed")
    var row = self
    row.fields[key] = value
    return row
  }
}

extension TimelineEntry: Codable {
  public init(from decoder: any Decoder) throws {
    let value = try JSONValue(from: decoder)
    guard case .object(let fields) = value else {
      throw DecodingError.dataCorrupted(.init(codingPath: decoder.codingPath, debugDescription: "a row is an object"))
    }
    try self.init(fields: fields)
  }

  public func encode(to encoder: any Encoder) throws {
    try JSONValue.object(fields).encode(to: encoder)
  }
}

/// One backward page of a session timeline, oldest-first within the page.
public struct TimelinePage: Sendable, Hashable, Codable {
  public var entries: [TimelineEntry]
  /// Projection generation shared by the snapshot, pages, and exact reads.
  public var watermark: String
  /// Opaque continuation for the next older page; nil at the top of the timeline.
  public var cursor: String?

  public init(entries: [TimelineEntry], watermark: String, cursor: String?) {
    self.entries = entries
    self.watermark = watermark
    self.cursor = cursor
  }
}

/// An exact row and the generation it belongs to.
public struct TimelineEntryResponse: Sendable, Hashable, Codable {
  public var entry: TimelineEntry
  public var watermark: String
}

/// Exact shell or code source recovered from the raw ledger only after an explicit disclosure. Timeline pages never
/// carry this payload.
public struct CommandResponse: Sendable, Hashable, Codable {
  public var command: String
}

/// A session's catalog row, kept whole.
public struct SessionResponse: Sendable, Hashable, Codable {
  public var session: JSONValue
}

/// The session's light work index. Each list's rows stay JSON. A list a daemon omits reads empty: a newly added
/// collection is the one backwards-compatible change the TypeScript client accepts across a rolling restart.
public struct TimelineMeta: Sendable, Hashable, Codable {
  public var tasks: [JSONValue]
  public var workflows: [JSONValue]
  public var plans: [JSONValue]
  public var artifacts: [JSONValue]

  public static let empty = TimelineMeta(tasks: [], workflows: [], plans: [], artifacts: [])

  public init(tasks: [JSONValue], workflows: [JSONValue], plans: [JSONValue], artifacts: [JSONValue]) {
    self.tasks = tasks
    self.workflows = workflows
    self.plans = plans
    self.artifacts = artifacts
  }

  private enum CodingKeys: String, CodingKey { case tasks, workflows, plans, artifacts }

  public init(from decoder: any Decoder) throws {
    let container = try decoder.container(keyedBy: CodingKeys.self)
    tasks = try container.decodeIfPresent([JSONValue].self, forKey: .tasks) ?? []
    workflows = try container.decodeIfPresent([JSONValue].self, forKey: .workflows) ?? []
    plans = try container.decodeIfPresent([JSONValue].self, forKey: .plans) ?? []
    artifacts = try container.decodeIfPresent([JSONValue].self, forKey: .artifacts) ?? []
  }
}

/// One projected mutation, tagged by `type`. The session subscription acts on the first five; the rest are the meta
/// subscription's, or a newer daemon's, and are kept by their tag.
public enum Mutation: Sendable, Hashable {
  /// A whole row, new or replacing the row with its `timeline_id`.
  case message(TimelineEntry)
  /// A tool row's tool, replaced.
  case toolUpdated(sessionId: String, timelineId: String, tool: JSONValue, at: Int64?)
  /// A row's usage, replaced, with the read-time cost of the replacement (nil: unpriced).
  case usage(sessionId: String, timelineId: String, ts: Int64?, model: String?, tokens: JSONValue, cost: JSONValue?)
  case sessionSwept(sessionId: String)
  case timelineMetaChanged(sessionId: String)
  case other(type: String, fields: [String: JSONValue])

  /// The row a row mutation targets.
  public var timelineId: String? {
    switch self {
    case .message(let row): return row.timelineId
    case .toolUpdated(_, let id, _, _), .usage(_, let id, _, _, _, _): return id
    default: return nil
    }
  }
}

extension Mutation: Decodable {
  public init(from decoder: any Decoder) throws {
    guard case .object(var fields) = try JSONValue(from: decoder), let type = fields["type"]?.stringValue else {
      throw DecodingError.dataCorrupted(.init(codingPath: decoder.codingPath, debugDescription: "a mutation names its type"))
    }
    func string(_ key: String) throws -> String {
      guard let value = fields[key]?.stringValue else {
        throw DecodingError.dataCorrupted(
          .init(codingPath: decoder.codingPath, debugDescription: "a \(type) mutation names its \(key)"))
      }
      return value
    }
    switch type {
    case "message":
      fields["type"] = nil
      self = .message(try TimelineEntry(fields: fields))
    case "tool_updated":
      self = .toolUpdated(
        sessionId: try string("session_id"), timelineId: try string("timeline_id"), tool: fields["tool"] ?? .null,
        at: fields["at"]?.intValue)
    case "usage":
      let cost = fields["cost"]
      self = .usage(
        sessionId: try string("session_id"), timelineId: try string("timeline_id"), ts: fields["ts"]?.intValue,
        model: fields["model"]?.stringValue, tokens: fields["tokens"] ?? .null,
        cost: cost == nil || cost == .null ? nil : cost)
    case "session_swept":
      self = .sessionSwept(sessionId: try string("session_id"))
    case "timeline_meta_changed":
      self = .timelineMetaChanged(sessionId: try string("session_id"))
    default:
      self = .other(type: type, fields: fields)
    }
  }
}

/// A server→client WebSocket frame, tagged by `frame`. `Hello` advertises the target position; only an applied
/// snapshot or a complete replay (`caught_up`) acknowledges it. Mutations continue from there (at-least-once; keyed
/// by `timeline_id`), and `resync` announces a barrier: the server re-stamps and sends a fresh snapshot after it.
public enum ServerFrame: Sendable, Hashable {
  case hello(protocolVersion: Int, stamp: Stamp?)
  case timelineSnapshot(stamp: Stamp, entries: [TimelineEntry], timelineMeta: TimelineMeta, cursor: String?, watermark: String)
  case caughtUp(stamp: Stamp)
  case timelineMetaSnapshot(stamp: Stamp?, timelineMeta: TimelineMeta)
  case mutation(stamp: Stamp?, mutation: Mutation)
  case resync
  case error(ApiError)
  /// A frame of another subscription (collaboration, meta), or of a newer daemon: kept by its tag.
  case other(frame: String)
}

extension ServerFrame: Decodable {
  private enum Key: String, CodingKey {
    case frame, protocolVersion = "protocol_version", stamp, entries, timelineMeta = "timeline_meta", cursor
    case watermark, mutation, error
  }

  public init(from decoder: any Decoder) throws {
    let container = try decoder.container(keyedBy: Key.self)
    let frame = try container.decode(String.self, forKey: .frame)
    switch frame {
    case "hello":
      self = .hello(
        protocolVersion: try container.decode(Int.self, forKey: .protocolVersion),
        stamp: try container.decodeIfPresent(Stamp.self, forKey: .stamp))
    case "timeline_snapshot":
      self = .timelineSnapshot(
        stamp: try container.decode(Stamp.self, forKey: .stamp),
        entries: try container.decode([TimelineEntry].self, forKey: .entries),
        timelineMeta: try container.decode(TimelineMeta.self, forKey: .timelineMeta),
        cursor: try container.decodeIfPresent(String.self, forKey: .cursor),
        watermark: try container.decode(String.self, forKey: .watermark))
    case "caught_up":
      self = .caughtUp(stamp: try container.decode(Stamp.self, forKey: .stamp))
    case "timeline_meta_snapshot":
      self = .timelineMetaSnapshot(
        stamp: try container.decodeIfPresent(Stamp.self, forKey: .stamp),
        timelineMeta: try container.decode(TimelineMeta.self, forKey: .timelineMeta))
    case "mutation":
      self = .mutation(
        stamp: try container.decodeIfPresent(Stamp.self, forKey: .stamp),
        mutation: try container.decode(Mutation.self, forKey: .mutation))
    case "resync":
      self = .resync
    case "error":
      self = .error(try container.decode(ApiError.self, forKey: .error))
    default:
      self = .other(frame: frame)
    }
  }
}
