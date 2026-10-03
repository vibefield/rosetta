import fs from "node:fs";
import ts from "typescript";
import { CursorExpiredError } from "../../../ts/client.ts";
import * as head from "../../renderer/src/lib/transcript/head-state.ts";
import * as historyWindow from "../../renderer/src/lib/transcript/history-window.ts";
import { createSessionLeases } from "../../renderer/src/lib/session-leases.ts";

// The hooks that read one session's body take the room's session leases; the
// rest take the catalog. A fake client stands in for both, as rosetta's app
// hands one client to both.
const SESSION_HOOKS = new Set(["useTimeline", "useTeamTimelines"]);

// Run the real data hooks with deterministic React scheduling. No DOM or
// network is needed to exercise effect cleanup and responses arriving late.
export function dataHooks(client) {
  const sessions = createSessionLeases({ open: () => client, close() {} });
  let active;
  const react = {
    useState(initial) {
      const runtime = active;
      const slot = runtime.slot(() => ({
        value: typeof initial === "function" ? initial() : initial,
      }));
      return [
        slot.value,
        (value) => {
          const next = typeof value === "function" ? value(slot.value) : value;
          if (!Object.is(next, slot.value)) {
            slot.value = next;
            runtime.dirty = true;
          }
        },
      ];
    },
    useRef(initial) {
      return active.slot(() => ({ current: initial }));
    },
    useMemo(make, deps) {
      const slot = active.slot(() => ({}));
      if (!same(slot.deps, deps)) {
        slot.value = make();
        slot.deps = deps;
      }
      return slot.value;
    },
    useCallback(callback, deps) {
      return react.useMemo(() => callback, deps);
    },
    useEffect(effect, deps) {
      const slot = active.slot(() => ({}));
      if (!same(slot.deps, deps)) {
        slot.deps = deps;
        active.effects.push(() => {
          slot.cleanup?.();
          slot.cleanup = effect();
        });
      }
    },
  };
  const dependencies = {
    react,
    "@rosetta/protocol": { CursorExpiredError },
    "./model": { getActivityGridStartTimestamp: () => 0, getViewerUtcOffsetMinutes: () => 0 },
    "./transcript/head-state": head,
    "./transcript/history-window": historyWindow,
  };
  const source = fs.readFileSync(
    new URL("../../renderer/src/lib/hooks.ts", import.meta.url),
    "utf8",
  );
  const { outputText } = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  });
  const exports = {};
  new Function("require", "exports", outputText)((name) => {
    if (!(name in dependencies)) throw new Error(`Unexpected dependency: ${name}`);
    return dependencies[name];
  }, exports);
  return (name, ...initial) => {
    const slots = [];
    let args = initial;
    let result;
    const renders = [];
    const runtime = {
      index: 0,
      dirty: true,
      effects: [],
      slot(make) {
        const i = this.index++;
        return (slots[i] ??= make());
      },
    };
    return {
      renders,
      render(...next) {
        if (next.length) args = next;
        let turns = 0;
        do {
          if (++turns > 50) throw new Error("Hook did not settle");
          runtime.index = 0;
          runtime.dirty = false;
          active = runtime;
          result = exports[name](SESSION_HOOKS.has(name) ? sessions : client, ...args);
          renders.push(result);
          active = null;
          for (const effect of runtime.effects.splice(0)) effect();
        } while (runtime.dirty);
        return result;
      },
      close() {
        for (const slot of slots) slot.cleanup?.();
      },
    };
  };
}
function same(a, b) {
  return (
    a !== undefined &&
    b !== undefined &&
    a.length === b.length &&
    a.every((value, i) => Object.is(value, b[i]))
  );
}
export async function microtasks() {
  for (let i = 0; i < 15; i++) await Promise.resolve();
}
