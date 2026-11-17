// A JSON value, kept whole. The rows this client carries are the daemon's own: it reads the few fields the
// subscribe protocol keys on (identity, ordering, the patch targets) and keeps every other field as it was sent, so
// a newer daemon's additions pass through to the consumer instead of being dropped by a stale type.

import Foundation

public enum JSONValue: Sendable, Hashable {
  case null
  case bool(Bool)
  /// Every integer the protocol sends stays below 2^53 (stamps, ordinals, epoch milliseconds).
  case int(Int64)
  case double(Double)
  case string(String)
  case array([JSONValue])
  case object([String: JSONValue])

  public subscript(key: String) -> JSONValue? {
    if case .object(let fields) = self { return fields[key] }
    return nil
  }

  public var stringValue: String? {
    if case .string(let value) = self { return value }
    return nil
  }

  public var intValue: Int64? {
    switch self {
    case .int(let value): return value
    case .double(let value) where value.rounded() == value && abs(value) < 9_007_199_254_740_992:
      return Int64(value)
    default: return nil
    }
  }

  public var isNull: Bool {
    if case .null = self { return true }
    return false
  }

  /// The members of an object, or nil for anything else.
  public var objectValue: [String: JSONValue]? {
    if case .object(let fields) = self { return fields }
    return nil
  }

  public var arrayValue: [JSONValue]? {
    if case .array(let items) = self { return items }
    return nil
  }
}

extension JSONValue: Codable {
  public init(from decoder: any Decoder) throws {
    let container = try decoder.singleValueContainer()
    if container.decodeNil() {
      self = .null
    } else if let value = try? container.decode(Bool.self) {
      self = .bool(value)
    } else if let value = try? container.decode(Int64.self) {
      self = .int(value)
    } else if let value = try? container.decode(Double.self) {
      self = .double(value)
    } else if let value = try? container.decode(String.self) {
      self = .string(value)
    } else if let value = try? container.decode([JSONValue].self) {
      self = .array(value)
    } else {
      self = .object(try container.decode([String: JSONValue].self))
    }
  }

  public func encode(to encoder: any Encoder) throws {
    var container = encoder.singleValueContainer()
    switch self {
    case .null: try container.encodeNil()
    case .bool(let value): try container.encode(value)
    case .int(let value): try container.encode(value)
    case .double(let value): try container.encode(value)
    case .string(let value): try container.encode(value)
    case .array(let value): try container.encode(value)
    case .object(let value): try container.encode(value)
    }
  }
}
