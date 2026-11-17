// A row's stored members: the content-addressed assets a timeline names (`AssetRef`, `ImageVariant` in the Rust
// core), hand-written like the other wire types and held to them by the wire golden. A row's body stays whole JSON;
// a consumer that wants a member reads it out of the body with `AssetRef(_:)`.

import Foundation

/// The finite image recipes the daemon encodes. Each is a WebP; `full` is the image at its own size.
public enum ImageVariant: String, Sendable, Hashable, CaseIterable, Codable {
  case full
  case rail
  case tile
  case tile2x
  case preview
}

/// One stored member: its address on the daemon, its content digest, and, for an image, its sized renditions.
public struct AssetRef: Sendable, Hashable, Decodable {
  /// The member's address, relative to the daemon's base URL.
  public var url: String
  public var digest: String
  public var mediaType: String
  public var byteLen: Int64
  /// Sized images by variant name. A name this build does not know is kept and never asked for.
  public var imageVariants: [String: AssetRef]?

  public init(
    url: String, digest: String, mediaType: String, byteLen: Int64, imageVariants: [String: AssetRef]? = nil
  ) {
    self.url = url
    self.digest = digest
    self.mediaType = mediaType
    self.byteLen = byteLen
    self.imageVariants = imageVariants
  }

  /// The member a row's JSON names, or nil when the value is not one.
  public init?(_ json: JSONValue) {
    guard let url = json["url"]?.stringValue, let digest = json["digest"]?.stringValue,
      let mediaType = json["media_type"]?.stringValue, let byteLen = json["byte_len"]?.intValue
    else { return nil }
    var variants: [String: AssetRef]?
    if let members = json["image_variants"]?.objectValue {
      variants = members.compactMapValues { AssetRef($0) }
    }
    self.init(url: url, digest: digest, mediaType: mediaType, byteLen: byteLen, imageVariants: variants)
  }

  private enum CodingKeys: String, CodingKey {
    case url, digest
    case mediaType = "media_type"
    case byteLen = "byte_len"
    case imageVariants = "image_variants"
  }
}

/// An image as the daemon answered it.
public struct ImageData: Sendable, Hashable {
  public var bytes: Data
  /// Always `image/webp`: every display address is a WebP the daemon encoded.
  public var contentType: String
  /// The answer's validator, when it carried one. A sized member's own address carries none: its digest is in its
  /// name. Both kinds of address are immutable, so an image is cached by its asset's digest and variant.
  public var etag: String?

  public init(bytes: Data, contentType: String, etag: String?) {
    self.bytes = bytes
    self.contentType = contentType
    self.etag = etag
  }
}
