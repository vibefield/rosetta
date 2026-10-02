import { useState } from "react";
import type { MetaState } from "@rosetta/protocol";
import { advanceActivityOrder } from "./activity-order.ts";

export function useActivityOrder(
  meta: MetaState | null,
  snapshotVersion: number,
  live: boolean,
  bootstrapping: boolean,
  now: number,
) {
  const [state, setState] = useState(() => ({
    meta, snapshotVersion, live, bootstrapping,
    order: advanceActivityOrder(null, meta?.sessions ?? [], { now, live: false, bootstrapping }),
  }));
  if (state.meta !== meta || state.snapshotVersion !== snapshotVersion ||
      state.live !== live || state.bootstrapping !== bootstrapping) {
    const next = {
      meta, snapshotVersion, live, bootstrapping,
      order: advanceActivityOrder(state.order, meta?.sessions ?? [], {
        now, bootstrapping,
        live: live && state.live && state.snapshotVersion === snapshotVersion,
      }),
    };
    // Pure render-time adjustment keeps ordering and rows in the same commit;
    // no effect publishes a stale intermediate order or mutates a render ref.
    setState(next);
    return next.order;
  }
  return state.order;
}
