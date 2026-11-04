// The simulated daemon's REST doors, as a `fetch` a host's RosettaClient takes
// (`RosettaClientOptions.fetchFn`): the routes and the wire shapes rosettad
// answers (crates/server/src/http.rs), each JSON body stamped with the
// protocol version the SDK checks, each refusal the daemon's error body. A
// host that builds the room's ports over its own client (rosetta's app:
// `catalogOver`, the roll call) then runs its real code against the
// simulator; only the sockets stay the simulator's (`cold-import.ts`).
import { PROTOCOL_VERSION } from "@vibefield/rosetta-protocol";
import type {
  ArtifactPage,
  BoostResponse,
  ContributionResponse,
  DayUsageResponse,
  DiscoveriesResponse,
  EnableResponse,
  ErrorCode,
  HeatmapResponse,
  SearchResponse,
  StatusResponse,
  UsageResponse,
} from "@vibefield/rosetta-protocol";
import type { CatalogSearch } from "../ports";

/** A refusal, as the daemon words it (`ApiError`). */
export class ColdWireError extends Error {
  readonly status: number;
  readonly code: ErrorCode;
  constructor(status: number, code: ErrorCode, message: string) {
    super(message);
    this.status = status;
    this.code = code;
  }
}

/** What the simulated daemon answers, route by route. */
export interface ColdWire {
  /** May be held by the controller; rejects with an AbortError on abort. */
  status(signal?: AbortSignal): Promise<StatusResponse>;
  discoveries(): DiscoveriesResponse;
  enableRuntimes(kinds: readonly string[]): EnableResponse;
  heatmap(projectId: string): HeatmapResponse;
  contribution(projectId: string): ContributionResponse;
  usage(projectId: string): UsageResponse;
  dayUsage(projectId: string, day: string): DayUsageResponse;
  search(request: CatalogSearch, signal?: AbortSignal): SearchResponse;
  boost(sessionId: string): BoostResponse;
  artifacts(): ArtifactPage;
}

export const aborted = () => new DOMException("The ask was aborted", "AbortError");

function answer(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json", "x-rosetta-protocol-version": String(PROTOCOL_VERSION) },
  });
}

function bodyOf(init: RequestInit | undefined): Record<string, unknown> {
  return typeof init?.body === "string" ? (JSON.parse(init.body) as Record<string, unknown>) : {};
}

/** The routes the SDK asks (clients/ts/client.ts), answered over `wire`. */
export function fetchOver(wire: ColdWire): typeof fetch {
  const route = async (url: URL, init: RequestInit | undefined): Promise<unknown> => {
    const method = init?.method ?? "GET";
    const signal = init?.signal ?? undefined;
    const path = url.pathname.split("/").filter((part) => part !== "").map(decodeURIComponent);
    const query = (key: string) => url.searchParams.get(key);
    const at = `${method} /${path.join("/")}`;
    if (at === "GET /api/status") return wire.status(signal);
    if (at === "GET /api/discoveries") return wire.discoveries();
    if (at === "POST /api/runtimes/enable") return wire.enableRuntimes((bodyOf(init).kinds as string[] | undefined) ?? []);
    if (at === "POST /api/ingest/boost") return wire.boost(String(bodyOf(init).session_id ?? ""));
    if (at === "GET /api/artifacts") return wire.artifacts();
    if (at === "GET /api/search") {
      return wire.search({
        q: query("q") ?? "", limit: Number(query("limit") ?? 50), project: query("project"), kind: query("kind"), agent: query("agent"),
      }, signal);
    }
    if (method === "GET" && path[0] === "api" && path[1] === "projects" && path[2] !== undefined) {
      const projectId = path[2];
      const rest = path.slice(3).join("/");
      if (rest === "heatmap") return wire.heatmap(projectId);
      if (rest === "contribution") return wire.contribution(projectId);
      if (rest === "usage") return wire.usage(projectId);
      if (rest === "usage/day") return wire.dayUsage(projectId, query("day") ?? "");
    }
    throw new ColdWireError(404, "not_found", `The simulated daemon has no route ${at}`);
  };
  return async (input, init) => {
    const href = typeof input === "string" ? input : input instanceof URL ? input.href : input.url;
    if (init?.signal?.aborted) throw aborted();
    try {
      const body = await route(new URL(href), init);
      if (init?.signal?.aborted) throw aborted();
      return answer(200, body);
    } catch (error) {
      if (error instanceof ColdWireError) return answer(error.status, { error: { code: error.code, message: error.message } });
      throw error;
    }
  };
}
