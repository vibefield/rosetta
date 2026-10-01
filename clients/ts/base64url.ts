// base64url without padding (RFC 4648 §5). Decoding is strict — the
// alphabet only, no padding, zero trailing bits — so every byte string has
// exactly one spelling, the rule the daemon's decoder applies to grants.
// Browser-safe: the client encodes a bearer with it, grants.ts both ways.

const ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
const VALUES = new Map<string, number>([...ALPHABET].map((char, index) => [char, index]));

function digit(value: number): string {
  return ALPHABET.charAt(value & 63);
}

export function base64UrlEncode(bytes: Uint8Array): string {
  let text = "";
  for (let at = 0; at < bytes.length; at += 3) {
    const chunk = ((bytes[at] ?? 0) << 16) | ((bytes[at + 1] ?? 0) << 8) | (bytes[at + 2] ?? 0);
    const width = Math.min(3, bytes.length - at);
    text += digit(chunk >> 18) + digit(chunk >> 12);
    if (width > 1) text += digit(chunk >> 6);
    if (width > 2) text += digit(chunk);
  }
  return text;
}

/** The bytes, or `null` for anything a strict decoder refuses. */
export function base64UrlDecode(text: string): Uint8Array | null {
  if (text.length % 4 === 1) return null;
  const bytes = new Uint8Array(Math.floor((text.length * 3) / 4));
  let buffer = 0;
  let bits = 0;
  let index = 0;
  for (const char of text) {
    const value = VALUES.get(char);
    if (value === undefined) return null;
    buffer = (buffer << 6) | value;
    bits += 6;
    if (bits >= 8) {
      bits -= 8;
      bytes[index++] = (buffer >> bits) & 0xff;
      buffer &= (1 << bits) - 1;
    }
  }
  // Leftover bits of a canonical encoding are zero.
  return buffer === 0 ? bytes : null;
}

export function utf8Base64UrlEncode(text: string): string {
  return base64UrlEncode(new TextEncoder().encode(text));
}
