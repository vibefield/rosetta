// The room's ports over one RosettaClient: how rosetta's app (and any host
// that holds the daemon's address and needs no per-session credential) hands
// the room its catalog, its sessions and its assets. Pure: no window, no
// bridge — `rosetta-host.ts` does the wiring.
import type { AssetRef, RosettaClient } from "@rosetta/protocol";
import type { AssetSource, CatalogPort, SessionPort } from "../ports";
import { catalogStatusOf } from "../lib/catalog-status";

/** What the host knows about its daemon beyond the client: its own words for
 * why the daemon cannot answer, and when its route to it changes. */
export interface DaemonWords {
  unavailable(): string | null;
  onChange(listener: () => void): () => void;
}

const NO_WORDS: DaemonWords = { unavailable: () => null, onChange: () => () => {} };

/** The catalog over the daemon's own lists and figures: REST and /ws/meta. */
export function catalogOver(client: RosettaClient, words: DaemonWords = NO_WORDS): CatalogPort {
  return {
    subscribeMeta: (handlers) => client.subscribeMeta(handlers),
    status: async (signal) => catalogStatusOf(await client.status(signal)),
    onConnectionChange: (listener) => {
      const removeConnection = client.onConnectionChange(listener);
      const removeHost = words.onChange(listener);
      return () => {
        removeConnection();
        removeHost();
      };
    },
    unavailable: () => words.unavailable(),
    projectStats: async (projectId, window, signal) => {
      const [heat, contribution] = await Promise.all([
        client.heatmap(projectId, window, signal),
        client.contribution(projectId, signal),
      ]);
      return { heat: heat.days, contribution: contribution.rows };
    },
    usage: (projectId, window, signal) => client.usage(projectId, window, signal),
    dayUsage: (projectId, day, offsetMinutes, signal) => client.dayUsage(projectId, day, offsetMinutes, signal),
    search: (request, signal) =>
      client.search(
        {
          q: request.q,
          limit: request.limit,
          session: null,
          sessions: null,
          prefix: null,
          runtime: null,
          agent: request.agent,
          project: request.project,
          kind: request.kind,
          is_error: null,
          after_ts: null,
          before_ts: null,
        },
        signal,
      ),
    boost: (sessionId, signal) => client.boost(sessionId, signal),
  };
}

/** One client reads every session: this daemon wants no per-session
 * credential, so opening is handing it over and closing is nothing. */
export function sessionsOver(client: RosettaClient): SessionPort {
  return { open: () => client, close: () => {} };
}

/** The daemon's own URL for a stored member, as long as it names that daemon. */
export function sameOriginAssetUrl(client: Pick<RosettaClient, "baseUrl">, asset: AssetRef): string | null {
  try {
    const url = new URL(asset.url, `${client.baseUrl}/`);
    return (url.protocol === "http:" || url.protocol === "https:") && url.origin === new URL(client.baseUrl).origin
      ? url.toString()
      : null;
  } catch {
    return null;
  }
}

/** Every image and attachment by the daemon's own URL: this daemon serves
 * an `<img src>` without a credential. */
export function assetsOver(client: RosettaClient): AssetSource {
  return {
    imageAssetUrl: (asset, variant) => client.imageAssetUrl(asset, variant),
    attachmentAssetUrl: (asset) => sameOriginAssetUrl(client, asset),
  };
}
