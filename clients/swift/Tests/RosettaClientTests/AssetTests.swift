// A row's stored members, against an HTTP stub: where an image is asked (`imageAssetUrl` in the TypeScript client),
// what comes back, and the request rules on a path that answers bytes.

import Foundation
import Testing

@testable import RosettaClient

extension ClientTests {
  @Suite("A stored member is asked at the address the TypeScript client would draw", .serialized)
  struct StoredMembers {
    private static let base = "http://127.0.0.1:4640"
    private static let webp = Data([0x52, 0x49, 0x46, 0x46, 0x00, 0x00, 0x00, 0x00, 0x57, 0x45, 0x42, 0x50])

    private func client(_ credential: RosettaCredential? = .grant("g1")) -> RosettaClient {
      RosettaClient(
        baseURL: URL(string: Self.base)!, session: StubHTTP.session(), credential: credential,
        sockets: ScriptedSockets())
    }

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

    private static func name(_ digit: Character, _ ext: String) -> String {
      "/api/assets/\(String(repeating: digit, count: 64)).\(ext)"
    }

    private static func member(_ digit: Character, _ ext: String, _ mediaType: String) -> AssetRef {
      AssetRef(
        url: name(digit, ext), digest: String(repeating: digit, count: 64), mediaType: mediaType, byteLen: 12)
    }

    /// A WebP with its sized members, as the daemon stores an image it encoded.
    private static let bundle: AssetRef = {
      var asset = StoredMembers.member("a", "webp", "image/webp")
      asset.imageVariants = [
        "tile": StoredMembers.member("b", "webp", "image/webp"),
        "rail": StoredMembers.member("c", "webp", "image/webp"),
      ]
      return asset
    }()

    /// A recorded original: an image with no WebP bundle.
    private static let original = StoredMembers.member("d", "png", "image/png")

    @Test("a WebP bundle is asked at its member's own address, the full image at the asset's, with no query")
    func bundleMembers() throws {
      let reader = client()
      #expect(
        try reader.imageAssetURL(Self.bundle, variant: .tile).absoluteString == Self.base + Self.name("b", "webp"))
      #expect(
        try reader.imageAssetURL(Self.bundle, variant: .full).absoluteString == Self.base + Self.name("a", "webp"))
    }

    @Test("a recorded original, and a bundle missing the variant, are asked through the versioned recipe")
    func recipes() throws {
      let reader = client()
      for variant in ImageVariant.allCases {
        #expect(
          try reader.imageAssetURL(Self.original, variant: variant).absoluteString
            == Self.base + Self.name("d", "png") + "?image=\(variant.rawValue)&v=1")
      }
      #expect(
        try reader.imageAssetURL(Self.bundle, variant: .preview).absoluteString
          == Self.base + Self.name("a", "webp") + "?image=preview&v=1")
    }

    @Test("an address on another origin, or one that is not http, is refused before any request")
    func foreignAddresses() async {
      let reader = client()
      for address in ["http://127.0.0.1:4641/api/assets/x.webp", "https://127.0.0.1:4640/x.webp", "file:///x.webp"] {
        var asset = Self.original
        asset.url = address
        let error = await failure { _ = try await reader.image(asset, variant: .tile) }
        #expect(
          error == .request(status: 0, code: .badRequest, message: "the stored image has an unsupported address"))
      }
      #expect(StubHTTP.requests.withLock { $0.isEmpty })
    }

    @Test("an image comes back as its bytes, content type and validator, asked with the credential")
    func imageBytes() async throws {
      StubHTTP.answer = { _ in StubHTTP.bytes(Self.webp, contentType: "image/webp", headers: ["etag": "\"e1\""]) }
      let image = try await client().image(Self.original, variant: .tile)
      #expect(image == ImageData(bytes: Self.webp, contentType: "image/webp", etag: "\"e1\""))
      let asked = try #require(StubHTTP.requests.withLock { $0.last })
      #expect(asked.url?.absoluteString == Self.base + Self.name("d", "png") + "?image=tile&v=1")
      #expect(asked.value(forHTTPHeaderField: "authorization") == "Rosetta-Grant g1")
      #expect(asked.value(forHTTPHeaderField: "if-none-match") == nil)
    }

    @Test("a member's own address answers with no validator")
    func noValidator() async throws {
      StubHTTP.answer = { _ in StubHTTP.bytes(Self.webp, contentType: "image/webp") }
      let image = try await client().image(Self.bundle, variant: .rail)
      #expect(image.etag == nil)
      #expect(StubHTTP.requests.withLock { $0.last }?.url?.absoluteString == Self.base + Self.name("c", "webp"))
    }

    @Test("an answer that is not a WebP is refused")
    func notARendition() async {
      StubHTTP.answer = { _ in StubHTTP.bytes(Data([0x89, 0x50, 0x4E, 0x47]), contentType: "image/png") }
      let error = await failure { _ = try await client().image(Self.original, variant: .full) }
      #expect(error?.message.contains("did not answer with an image rendition") == true)
    }

    @Test("the request rules hold where the answer is bytes")
    func requestRules() async {
      StubHTTP.answer = { _ in StubHTTP.Answer(status: 200, headers: ["content-type": "image/webp"], body: Self.webp) }
      let unversioned = await failure { _ = try await client().image(Self.original, variant: .tile) }
      #expect(unversioned?.code == .incompatibleProtocol)

      StubHTTP.answer = { _ in StubHTTP.json(401, .object([:]), headers: ["x-rosetta-auth": "expired"]) }
      let refused = await failure { _ = try await client().image(Self.original, variant: .tile) }
      #expect(
        refused
          == .unauthorized(
            reason: .expired, message: "The daemon refused \(Self.name("d", "png"))?image=tile&v=1 (expired)."))

      StubHTTP.answer = { _ in
        let error: JSONValue = .object(["code": .string("not_found"), "message": .string("no such projection asset")])
        return StubHTTP.json(404, .object(["error": error]))
      }
      #expect(
        await failure { _ = try await client().image(Self.original, variant: .tile) }
          == .request(status: 404, code: .notFound, message: "no such projection asset"))
    }

    @Test("a text member reads as its words; an image is not asked as text")
    func text() async throws {
      StubHTTP.answer = { _ in StubHTTP.bytes(Data("the whole output".utf8), contentType: "text/plain; charset=utf-8") }
      let member = Self.member("e", "txt", "text/plain; charset=utf-8")
      let reader = client()
      #expect(try await reader.readTextAsset(member) == "the whole output")
      #expect(StubHTTP.requests.withLock { $0.last }?.url?.absoluteString == Self.base + Self.name("e", "txt"))

      let error = await failure { _ = try await reader.readTextAsset(Self.original) }
      #expect(error == .request(status: 0, code: .badRequest, message: "the stored member is not text"))
      #expect(StubHTTP.requests.withLock { $0.count } == 1, "the image was never asked")
    }

    /// The size a `tooLarge` names and its bound, or nil for any other outcome.
    private func size(_ error: RosettaError?) -> (byteLen: Int64?, limit: Int)? {
      guard let error, case .tooLarge(let byteLen, let limit, _) = error else { return nil }
      return (byteLen, limit)
    }

    @Test("a text member past the bound is refused: by its own count before a request, by the answer's after")
    func textBound() async {
      var member = Self.member("e", "txt", "text/plain; charset=utf-8")
      let reader = client()
      let known = size(await failure { _ = try await reader.readTextAsset(member, maxBytes: 8) })
      #expect(known?.byteLen == 12 && known?.limit == 8)
      #expect(StubHTTP.requests.withLock { $0.isEmpty })

      member.byteLen = 8
      StubHTTP.answer = { _ in StubHTTP.bytes(Data("nine byte".utf8), contentType: "text/plain; charset=utf-8") }
      let counted = size(await failure { _ = try await reader.readTextAsset(member, maxBytes: 8) })
      #expect(counted?.limit == 8)
      #expect(StubHTTP.requests.withLock { $0.count } == 1)
    }

    @Test("an image whose stated length is past the bound is refused with that length")
    func statedLength() async {
      StubHTTP.answer = { _ in
        StubHTTP.bytes(Self.webp, contentType: "image/webp", headers: ["content-length": "\(Self.webp.count)"])
      }
      let refused = size(await failure { _ = try await client().image(Self.original, variant: .tile, maxBytes: 4) })
      #expect(refused?.byteLen == Int64(Self.webp.count) && refused?.limit == 4)
    }

    @Test("an image that states no length is refused once its count passes the bound, its size unknown")
    func countedLength() async throws {
      StubHTTP.answer = { _ in StubHTTP.bytes(Self.webp, contentType: "image/webp") }
      let refused = size(await failure { _ = try await client().image(Self.original, variant: .tile, maxBytes: 4) })
      #expect(refused?.limit == 4)
      #expect(refused?.byteLen == nil)
      // and at the bound exactly, it is read
      let image = try await client().image(Self.original, variant: .tile, maxBytes: Self.webp.count)
      #expect(image.bytes == Self.webp)
    }

    @Test("an expired grant is the same refusal on every read: the timeline's")
    func expiredGrant() async {
      StubHTTP.answer = { _ in StubHTTP.json(401, .object([:]), headers: ["x-rosetta-auth": "expired"]) }
      let reader = client()
      func reason(_ ask: () async throws -> Void) async -> AuthRefusal? {
        guard let refusal = await failure(ask), case .unauthorized(let reason, _) = refusal else { return nil }
        return reason
      }
      #expect(await reason { _ = try await reader.timeline("s1") } == .expired)
      #expect(await reason { _ = try await reader.image(Self.original, variant: .tile) } == .expired)
      #expect(
        await reason { _ = try await reader.readTextAsset(Self.member("e", "txt", "text/plain")) } == .expired)
      #expect(await reason { _ = try await reader.command("s1", timelineId: "t1") } == .expired)
    }

    @Test("the reads belong to no actor, and what they take and return crosses tasks")
    func isolation() async throws {
      func sendable<T: Sendable>(_: T.Type) {}
      sendable(AssetRef.self)
      sendable(ImageVariant.self)
      sendable(ImageData.self)
      sendable(CommandResponse.self)
      StubHTTP.answer = { _ in StubHTTP.bytes(Self.webp, contentType: "image/webp") }
      let reader = client()
      let read: @Sendable () async throws -> ImageData = { try await reader.image(Self.original, variant: .rail) }
      let image = try await Task.detached(operation: read).value
      #expect(image.bytes == Self.webp)
    }

    @Test("a read cancelled with its task throws, and not as the client's refusal")
    func cancellation() async throws {
      StubHTTP.answer = { _ in
        var never = StubHTTP.bytes(Data(), contentType: "image/webp")
        never.stalls = true
        return never
      }
      let reader = client()
      let read = Task { try await reader.image(Self.original, variant: .tile) }
      let clock = ContinuousClock()
      let deadline = clock.now + .seconds(5)
      while StubHTTP.requests.withLock({ $0.isEmpty }), clock.now < deadline {
        try await Task.sleep(for: .milliseconds(5))
      }
      read.cancel()
      do {
        _ = try await read.value
        Issue.record("the cancelled read answered")
      } catch is RosettaError {
        Issue.record("the cancelled read threw the client's refusal")
      } catch {
        // URLSession's own cancellation
      }
    }

    @Test("a recovered command is asked at its encoded path with the credential; past the bound it is refused")
    func command() async throws {
      StubHTTP.answer = { _ in StubHTTP.json(200, golden("command")) }
      let reader = client()
      let recovered = try await reader.command("s/1", timelineId: "t 1")
      #expect(recovered.command == golden("command")["command"]?.stringValue)
      let asked = try #require(StubHTTP.requests.withLock { $0.last })
      #expect(asked.url?.absoluteString == Self.base + "/api/sessions/s%2F1/timeline/t%201/command")
      #expect(asked.value(forHTTPHeaderField: "authorization") == "Rosetta-Grant g1")
      #expect(size(await failure { _ = try await reader.command("s1", timelineId: "t1", maxBytes: 8) })?.limit == 8)
    }
  }
}
