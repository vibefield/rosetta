// The two `node:crypto` functions grants.ts uses, declared here so
// `tsc -p clients/ts` needs no @types/node. A consumer with @types/node
// never sees this file (nothing references it); it type-checks grants.ts
// against the real declarations, which these are a subset of.
declare module "node:crypto" {
  interface Hmac {
    update(data: Uint8Array): Hmac;
    digest(): Uint8Array;
  }
  export function createHmac(algorithm: "sha256", key: Uint8Array): Hmac;
  export function timingSafeEqual(a: Uint8Array, b: Uint8Array): boolean;
}
