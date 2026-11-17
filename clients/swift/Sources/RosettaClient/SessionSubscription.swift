// One session's live timeline: the TypeScript client's `BaseSubscription` and `SessionSubscription`
// (`clients/ts/client.ts`), the one place the subscribe state machine lives outside Rust. Snapshot at a stamp, apply
// mutations keyed by `timeline_id` (patches into earlier rows included), obey `resync` barriers, reconnect with a
// resume stamp, and hold pages a reader fetched without showing them until it pulls. Consumers render; they never
// re-implement protocol semantics.
//
// Its state is the main actor's, as the TypeScript client's is its one thread's, and its handlers are called there.
// One difference, said: the TypeScript client says `syncing` when its socket opens; a URLSession WebSocket reports no
// open without a delegate, so this one says it at the daemon's hello, the first thing an open socket hears.

import Foundation

public enum SubscriptionStatus: String, Sendable {
  case connecting, syncing, live, resyncing, closed
}

public struct SessionSubscriptionHandlers {
  /// Every replacement invalidates all loaded history, anchors, and in-flight reads.
  public var onReset: (@MainActor () -> Void)?
  public var onSnapshot: (@MainActor (TimelinePage) -> Void)?
  /// The full ordered visible timeline after each coalesced change.
  public var onChange: (@MainActor ([TimelineEntry]) -> Void)?
  /// The complete light work index. Bodies remain behind lazy references.
  public var onTimelineMetaChange: (@MainActor (TimelineMeta) -> Void)?
  public var onStatus: (@MainActor (SubscriptionStatus) -> Void)?
  public var onError: (@MainActor (ApiError) -> Void)?

  public init(
    onReset: (@MainActor () -> Void)? = nil, onSnapshot: (@MainActor (TimelinePage) -> Void)? = nil,
    onChange: (@MainActor ([TimelineEntry]) -> Void)? = nil,
    onTimelineMetaChange: (@MainActor (TimelineMeta) -> Void)? = nil,
    onStatus: (@MainActor (SubscriptionStatus) -> Void)? = nil, onError: (@MainActor (ApiError) -> Void)? = nil
  ) {
    self.onReset = onReset
    self.onSnapshot = onSnapshot
    self.onChange = onChange
    self.onTimelineMetaChange = onTimelineMetaChange
    self.onStatus = onStatus
    self.onError = onError
  }
}

private let reconnectBase = Duration.milliseconds(500)
private let reconnectMax = Duration.seconds(10)
/// Opening a socket is not synchronizing: one that is not live (or a resync not answered) by then is closed and
/// redialled on the backoff.
private let syncDeadline = Duration.seconds(15)
/// An overwhelmed read retries instead of silently dropping a late patch.
private let pendingReadPatchLimit = 4096

@MainActor
public final class SessionSubscription {
  public let sessionId: String
  public private(set) var status: SubscriptionStatus?

  private let client: RosettaClient
  private let handlers: SessionSubscriptionHandlers
  private var stamp: Stamp?
  private var closed = false
  private var socket: (any RosettaSocket)?
  /// Which socket is current: a replaced socket's late frames and its close are not this subscription's.
  private var socketSerial = 0
  private var greeted = false
  private var attempts = 0
  private var reconnectTask: Task<Void, Never>?
  private var deadlineTask: Task<Void, Never>?
  private var changeTask: Task<Void, Never>?

  private var rows: [String: TimelineEntry] = [:]
  private var visible: [String: TimelineEntry] = [:]
  private var meta = TimelineMeta.empty
  private var generation = 0
  private var watermark: String?
  private var pending: [ObjectIdentifier: PendingRead] = [:]
  private var rowsDirty = false
  private var metaDirty = false

  init(client: RosettaClient, sessionId: String, handlers: SessionSubscriptionHandlers) {
    self.client = client
    self.sessionId = sessionId
    self.handlers = handlers
    connect()
  }

  deinit {
    socket?.close()
    reconnectTask?.cancel()
    deadlineTask?.cancel()
    changeTask?.cancel()
  }

  /// The visible timeline, ordered by ordinal, then by `timeline_id`.
  public var entries: [TimelineEntry] {
    visible.values.sorted { ($0.ordinal, $0.timelineId) < ($1.ordinal, $1.timelineId) }
  }

  public var timelineMeta: TimelineMeta { meta }

  // MARK: The socket

  private func connect() {
    guard !closed else { return }
    reconnectTask = nil
    let resume = stamp.map { "?epoch=\($0.epoch)&seq=\($0.seq)" } ?? ""
    setStatus(.connecting)
    guard let url = URL(string: client.socketURL("/ws/sessions/\(encodeComponent(sessionId))") + resume) else {
      retry()
      return
    }
    let opened = client.sockets.open(url: url, protocols: client.credential?.webSocketProtocols ?? [])
    socketSerial += 1
    let serial = socketSerial
    socket = opened
    greeted = false
    armDeadline(opened)
    Task { [weak self] in
      while true {
        let text: String
        do { text = try await opened.receive() } catch { break }
        guard let self, self.socketSerial == serial, !self.closed else { break }
        self.receive(text, on: opened)
      }
      self?.socketClosed(serial)
    }
  }

  private func receive(_ text: String, on opened: any RosettaSocket) {
    do {
      let frame = try JSONDecoder().decode(ServerFrame.self, from: Data(text.utf8))
      if case .hello(let version, _) = frame {
        guard version == Rosetta.protocolVersion else {
          onError(
            ApiError(
              code: .incompatibleProtocol,
              message: "Daemon protocol \(version) is incompatible with this app (expected "
                + "\(Rosetta.protocolVersion)). Restart with the matching daemon."))
          close()
          return
        }
        greeted = true
        setStatus(.syncing)
        // Hello advertises a target. It never acknowledges data we received.
        return
      }
      guard greeted else { throw FrameError(message: "The daemon sent data before its protocol handshake.") }
      if case .error(let error) = frame { onError(error) } else { handle(frame) }
    } catch {
      onError(ApiError(code: .internal, message: (error as? FrameError)?.message ?? "Invalid daemon frame"))
      stamp = nil
      invalidate()
      opened.close()
    }
  }

  private func socketClosed(_ serial: Int) {
    guard serial == socketSerial, socket != nil else { return }
    socket = nil
    clearDeadline()
    if !closed { retry() }
  }

  private func retry() {
    guard !closed else { return }
    setStatus(.connecting)
    let delay = min(reconnectMax, reconnectBase * (1 << min(attempts, 16)))
    attempts += 1
    reconnectTask = Task { [weak self] in
      try? await Task.sleep(for: delay)
      guard !Task.isCancelled else { return }
      self?.connect()
    }
  }

  private func armDeadline(_ opened: any RosettaSocket) {
    clearDeadline()
    deadlineTask = Task {
      try? await Task.sleep(for: syncDeadline)
      guard !Task.isCancelled else { return }
      opened.close()
    }
  }

  private func clearDeadline() {
    deadlineTask?.cancel()
    deadlineTask = nil
  }

  private func live() {
    attempts = 0
    clearDeadline()
    setStatus(.live)
  }

  private func setStatus(_ next: SubscriptionStatus) {
    guard status != next else { return }
    status = next
    if next == .resyncing, let socket { armDeadline(socket) }
    handlers.onStatus?(next)
  }

  private func changed(immediate: Bool = false) {
    guard !closed else { return }
    if immediate || client.notificationInterval == .zero {
      cancelChange()
      flushChanges()
    } else if changeTask == nil {
      let interval = client.notificationInterval
      changeTask = Task { [weak self] in
        try? await Task.sleep(for: interval)
        guard !Task.isCancelled, let self else { return }
        self.changeTask = nil
        if !self.closed { self.flushChanges() }
      }
    }
  }

  private func cancelChange() {
    changeTask?.cancel()
    changeTask = nil
  }

  /// Drop a generation and request an authoritative snapshot.
  public func restart() {
    guard !closed else { return }
    stamp = nil
    invalidate()
    let old = socket
    socket = nil
    clearDeadline()
    reconnectTask?.cancel()
    reconnectTask = nil
    old?.close()
    connect()
  }

  public func close() {
    guard !closed else { return }
    closed = true
    clearDeadline()
    cancelChange()
    reconnectTask?.cancel()
    reconnectTask = nil
    let old = socket
    socket = nil
    old?.close()
    dispose()
    setStatus(.closed)
  }

  // MARK: The timeline

  private func dispose() {
    for read in pending.values { read.abort() }
    pending.removeAll()
  }

  private func invalidate() {
    let hadSnapshot = watermark != nil
    cancelChange()
    generation += 1
    watermark = nil
    dispose()
    rows.removeAll()
    visible.removeAll()
    meta = .empty
    rowsDirty = false
    metaDirty = false
    if hadSnapshot { handlers.onReset?() }
  }

  private func handle(_ frame: ServerFrame) {
    switch frame {
    case .timelineSnapshot(let stamp, let entries, let timelineMeta, let cursor, let watermark):
      invalidate()
      self.watermark = watermark
      for row in entries {
        rows[row.timelineId] = row
        visible[row.timelineId] = row
      }
      meta = timelineMeta
      self.stamp = stamp
      handlers.onSnapshot?(TimelinePage(entries: entries, watermark: watermark, cursor: cursor))
      rowsDirty = true
      metaDirty = true
      live()
      changed(immediate: true)
    case .caughtUp(let stamp):
      // A partial replay leaves the stamp unchanged and is replayed again.
      self.stamp = stamp
      live()
      changed(immediate: true)
    case .timelineMetaSnapshot(let stamp, let timelineMeta):
      meta = timelineMeta
      metaDirty = true
      if let stamp {
        self.stamp = stamp
        live()
      }
      changed()
    case .mutation(let stamp, let mutation):
      if case .sessionSwept = mutation {
        restart()
        return
      }
      apply(mutation)
      if let stamp {
        self.stamp = stamp
        live()
      }
      changed()
    case .resync:
      stamp = nil
      invalidate()
      setStatus(.resyncing)
    default:
      break
    }
  }

  private func apply(_ mutation: Mutation) {
    guard let id = mutation.timelineId else { return }
    for read in pending.values { read.note(mutation) }
    let current = rows[id]
    guard let row = patchRow(current, mutation), row != current else { return }
    rows[row.timelineId] = row
    var isMessage = false
    if case .message = mutation { isMessage = true }
    if visible[row.timelineId] != nil || (current == nil && isMessage) {
      visible[row.timelineId] = row
      rowsDirty = true
    }
  }

  // MARK: Reads under the subscription

  /// Hold a page in this cache without displaying it until the reader pulls (`reveal`).
  public func fetchPage(cursor: String, limit: Int = 200) async throws -> TimelinePage {
    let client = self.client
    let sessionId = self.sessionId
    return try await read(show: false) { try await client.timeline(sessionId, limit: limit, cursor: cursor) }
  }

  /// Reveal cached identities using their latest patched values.
  public func reveal(_ entries: [TimelineEntry]) {
    for entry in entries {
      if let row = rows[entry.timelineId] { visible[entry.timelineId] = row }
    }
    rowsDirty = true
    changed(immediate: true)
  }

  @discardableResult
  public func loadEntry(_ timelineId: String) async throws -> Bool {
    let client = self.client
    let sessionId = self.sessionId
    _ = try await read(show: true) {
      let result = try await client.timelineEntry(sessionId, timelineId: timelineId)
      return TimelinePage(entries: [result.entry], watermark: result.watermark, cursor: nil)
    }
    return true
  }

  private func read(
    show: Bool, _ load: @escaping @Sendable () async throws -> TimelinePage
  ) async throws -> TimelinePage {
    guard !closed, watermark != nil else { throw RosettaError.synchronizing(message: "The session is synchronizing") }
    let generation = self.generation
    let read = PendingRead()
    let key = ObjectIdentifier(read)
    pending[key] = read
    defer { pending[key] = nil }
    let task = Task { try await load() }
    read.task = task
    do {
      let page = try await withTaskCancellationHandler {
        try await task.value
      } onCancel: {
        task.cancel()
      }
      if read.aborted { throw CancellationError() }
      guard generation == self.generation, !closed else {
        throw RosettaError.synchronizing(message: "The session changed")
      }
      guard page.watermark == watermark else { throw RosettaError.cursorExpired(message: "The session was rewritten") }
      for incoming in page.entries {
        var row: TimelineEntry? = incoming
        for mutation in read.mutations where mutation.timelineId == incoming.timelineId {
          row = patchRow(row, mutation)
        }
        guard let row else { continue }
        rows[row.timelineId] = row
        if show || visible[row.timelineId] != nil {
          visible[row.timelineId] = row
          rowsDirty = true
        }
      }
      changed(immediate: show)
      return page
    } catch let error as RosettaError {
      if case .cursorExpired(let message) = error, generation == self.generation {
        restart()
        handlers.onError?(ApiError(code: .cursorExpired, message: message))
      }
      throw error
    }
  }

  private func flushChanges() {
    if rowsDirty {
      rowsDirty = false
      handlers.onChange?(entries)
    }
    if metaDirty {
      metaDirty = false
      handlers.onTimelineMetaChange?(meta)
    }
  }

  private func onError(_ error: ApiError) {
    handlers.onError?(error)
  }
}

/// A read in flight under the subscription: the mutations that arrive while it loads, replayed onto its rows.
@MainActor
private final class PendingRead {
  var mutations: [Mutation] = []
  var task: Task<TimelinePage, any Error>?
  private(set) var aborted = false

  func note(_ mutation: Mutation) {
    if mutations.count >= pendingReadPatchLimit { abort() } else { mutations.append(mutation) }
  }

  func abort() {
    aborted = true
    task?.cancel()
  }
}

private struct FrameError: Error {
  let message: String
}
