// What a refused request or a daemon's error frame says, in the TypeScript client's shapes (`ApiRequestError`,
// `CursorExpiredError`, `UnauthorizedError`, the generated `ApiError`).

import Foundation

/// The protocol's `ErrorCode`, open to codes a newer daemon adds: a code this build does not name is kept as sent.
public struct ErrorCode: RawRepresentable, Sendable, Hashable, Codable {
  public let rawValue: String
  public init(rawValue: String) { self.rawValue = rawValue }
  public init(from decoder: any Decoder) throws {
    rawValue = try decoder.singleValueContainer().decode(String.self)
  }
  public func encode(to encoder: any Encoder) throws {
    var container = encoder.singleValueContainer()
    try container.encode(rawValue)
  }

  public static let incompatibleProtocol = ErrorCode(rawValue: "incompatible_protocol")
  public static let notFound = ErrorCode(rawValue: "not_found")
  public static let badRequest = ErrorCode(rawValue: "bad_request")
  public static let notReady = ErrorCode(rawValue: "not_ready")
  public static let cursorExpired = ErrorCode(rawValue: "cursor_expired")
  public static let indexChanged = ErrorCode(rawValue: "index_changed")
  public static let revisionConflict = ErrorCode(rawValue: "revision_conflict")
  public static let idempotencyConflict = ErrorCode(rawValue: "idempotency_conflict")
  public static let invalidTransition = ErrorCode(rawValue: "invalid_transition")
  public static let publicationInProgress = ErrorCode(rawValue: "publication_in_progress")
  public static let controlCapacity = ErrorCode(rawValue: "control_capacity")
  public static let shuttingDown = ErrorCode(rawValue: "shutting_down")
  public static let exportConflict = ErrorCode(rawValue: "export_conflict")
  public static let `internal` = ErrorCode(rawValue: "internal")

  /// Every code this build names, in the protocol's order.
  public static let known: [ErrorCode] = [
    .incompatibleProtocol, .notFound, .badRequest, .notReady, .cursorExpired, .indexChanged, .revisionConflict,
    .idempotencyConflict, .invalidTransition, .publicationInProgress, .controlCapacity, .shuttingDown,
    .exportConflict, .internal,
  ]
}

/// A daemon's error, as an error frame or an error body carries it. `details` stays JSON: only ingestion controls
/// fill it, and this client sends none.
public struct ApiError: Sendable, Hashable, Codable {
  public var code: ErrorCode
  public var message: String
  public var details: JSONValue?

  public init(code: ErrorCode, message: String, details: JSONValue? = nil) {
    self.code = code
    self.message = message
    self.details = details
  }
}

/// Why a request failed.
public enum RosettaError: Error, Sendable, Hashable {
  /// A non-success answer: its HTTP status, the daemon's code when it named one, and its message.
  case request(status: Int, code: ErrorCode?, message: String)
  /// The bound watermark no longer matches (HTTP 410): refetch from the head.
  case cursorExpired(message: String)
  /// A 401: the daemon wants a credential this client did not present, or refused the one it did. `reason` is nil
  /// only from a daemon that named none.
  case unauthorized(reason: AuthRefusal?, message: String)
  /// A read a subscription refused because its session is synchronizing, or changed under the read.
  case synchronizing(message: String)

  public var code: ErrorCode? {
    switch self {
    case .request(_, let code, _): return code
    case .cursorExpired: return ErrorCode.cursorExpired
    case .unauthorized, .synchronizing: return nil
    }
  }

  public var message: String {
    switch self {
    case .request(status: _, code: _, message: let message): return message
    case .cursorExpired(message: let message): return message
    case .unauthorized(reason: _, message: let message): return message
    case .synchronizing(message: let message): return message
    }
  }
}
