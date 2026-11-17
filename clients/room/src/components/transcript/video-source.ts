// One sent video's load, as a state machine with no React and no DOM: every
// door it uses is handed in, so a test drives it with fakes.
//
//   idle ── play ──▶ loading ──▶ ready(url) ──▶ (the element fails) ──▶ failed
//
// Nothing is asked before `play`. The host's own address comes first
// (`AssetSource.mediaAssetUrl`); when the host has none, answers none, or the
// element then cannot load it, the member's bytes are read through the
// reading session's client and played from an object URL made here. Only an
// object URL made here is revoked here: a host's address is the host's.
//
// A host's address is trusted to seek. One served whole, without byte ranges,
// plays in Chromium but cannot seek and raises no error to fall back on, so a
// host answers one only where ranges are honoured. rosetta's own app answers
// none: its videos take the bytes path, which seeks.
import type { AssetRef } from "@vibefield/rosetta-protocol";
import type { AssetUrl } from "../../ports";

export type VideoState =
  | { readonly phase: "idle" }
  | { readonly phase: "loading" }
  /** `from` says whose address plays: the host's, or the bytes read here. */
  | { readonly phase: "ready"; readonly url: string; readonly from: "host" | "bytes" }
  | { readonly phase: "failed" };

export const IDLE_VIDEO: VideoState = { phase: "idle" };

export interface VideoSourceDoors {
  /** The host's address for a `<video src>`; absent on a host that offers none. */
  readonly mediaAssetUrl?: ((asset: AssetRef) => AssetUrl) | undefined;
  /** The member's bytes, through the reading session's client. */
  readonly readAsset: (asset: AssetRef, signal: AbortSignal) => Promise<Uint8Array>;
  readonly createObjectURL: (bytes: Uint8Array, mediaType: string) => string;
  readonly revokeObjectURL: (url: string) => void;
  /** Every state after the first, in order. Never called after `dispose`. */
  readonly onChange?: ((state: VideoState) => void) | undefined;
}

export interface VideoSource {
  state(): VideoState;
  /** The reader asked to play, or to try again after a failure. A press while
   * loading or ready is nothing. */
  play(): void;
  /** The element could not load or decode `url`. The host's address falls
   * back to the bytes; the bytes' own failure is the failed face. A report
   * about an address that is no longer the current one is nothing. */
  elementFailed(url: string): void;
  /** Abort a read in flight and revoke any object URL made here. */
  dispose(): void;
}

export function createVideoSource(asset: AssetRef, doors: VideoSourceDoors): VideoSource {
  let state: VideoState = IDLE_VIDEO;
  let disposed = false;
  // Each press is its own run: an answer that arrives for an older run, or
  // after disposal, is dropped.
  let run = 0;
  let reading: AbortController | null = null;
  let objectUrl: string | null = null;

  const set = (next: VideoState) => {
    state = next;
    doors.onChange?.(next);
  };
  const current = (id: number) => !disposed && id === run;
  const release = () => {
    if (objectUrl === null) return;
    const url = objectUrl;
    objectUrl = null;
    doors.revokeObjectURL(url);
  };

  function readBytes(id: number): void {
    const controller = new AbortController();
    reading = controller;
    if (state.phase !== "loading") set({ phase: "loading" });
    const settle = () => {
      if (reading === controller) reading = null;
      return current(id);
    };
    let read: Promise<Uint8Array>;
    try {
      read = Promise.resolve(doors.readAsset(asset, controller.signal));
    } catch (error) {
      read = Promise.reject(error);
    }
    read.then(
      (bytes) => {
        if (!settle()) return;
        try {
          objectUrl = doors.createObjectURL(bytes, asset.media_type);
        } catch {
          set({ phase: "failed" });
          return;
        }
        set({ phase: "ready", url: objectUrl, from: "bytes" });
      },
      () => {
        if (settle()) set({ phase: "failed" });
      },
    );
  }

  return {
    state: () => state,
    play() {
      if (disposed || state.phase === "loading" || state.phase === "ready") return;
      const id = (run += 1);
      release();
      let answer: AssetUrl = null;
      try {
        if (doors.mediaAssetUrl !== undefined) answer = doors.mediaAssetUrl(asset);
      } catch {
        answer = null;
      }
      if (typeof answer === "string" && answer !== "") {
        set({ phase: "ready", url: answer, from: "host" });
        return;
      }
      if (answer === null || typeof answer === "string") {
        readBytes(id);
        return;
      }
      set({ phase: "loading" });
      Promise.resolve(answer).then(
        (url) => {
          if (!current(id)) return;
          if (typeof url === "string" && url !== "") set({ phase: "ready", url, from: "host" });
          else readBytes(id);
        },
        () => {
          if (current(id)) readBytes(id);
        },
      );
    },
    elementFailed(url) {
      if (disposed || state.phase !== "ready" || state.url !== url) return;
      if (state.from === "host") {
        readBytes(run);
        return;
      }
      release();
      set({ phase: "failed" });
    },
    dispose() {
      if (disposed) return;
      disposed = true;
      reading?.abort();
      reading = null;
      release();
    },
  };
}
