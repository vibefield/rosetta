import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import ts from "typescript";
import { ApiRequestError } from "../../ts/client.ts";
import { deferred } from "./support/protocol.mjs";
import { microtasks } from "./support/hooks.mjs";

const source = readFileSync(new URL("../../room/src/components/IngestionControls.tsx", import.meta.url), "utf8");
const { outputText } = ts.transpileModule(source, {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
});

// Exercise the production component with deterministic effects and deferred
// host replies. Like the data-hook tests, this needs neither DOM nor network.
function controls(run, ingestion) {
  const slots = [];
  let index = 0, dirty = true;
  const effects = [];
  const slot = (create) => (slots[index++] ??= create());
  const react = {
    useState(initial) {
      const state = slot(() => ({ value: initial }));
      return [state.value, (next) => {
        const value = typeof next === "function" ? next(state.value) : next;
        if (!Object.is(value, state.value)) { state.value = value; dirty = true; }
      }];
    },
    useRef: (initial) => slot(() => ({ current: initial })),
    useEffect(effect, deps) {
      const state = slot(() => ({}));
      if (!state.deps || deps.some((value, i) => !Object.is(value, state.deps[i]))) {
        state.deps = deps;
        effects.push(() => { state.cleanup?.(); state.cleanup = effect(); });
      }
    },
  };
  const element = (type, props) => ({ type, props });
  const dependencies = {
    react,
    "react/jsx-runtime": { jsx: element, jsxs: element },
    "@vibefield/rosetta-protocol": { ApiRequestError },
    "../room-context": { useRoom: () => ({ ingestion }) },
  };
  const exports = {};
  new Function("require", "exports", outputText)((name) => {
    assert.ok(name in dependencies, `Unexpected dependency: ${name}`);
    return dependencies[name];
  }, exports);
  return {
    render(next = {}) {
      if ("run" in next) run = next.run;
      if ("ingestion" in next) ingestion = next.ingestion;
      let result;
      let renders = 0;
      do {
        assert.ok(++renders < 30, "component settles");
        index = 0; dirty = false;
        result = exports.IngestionControls({ run });
        for (const effect of effects.splice(0)) effect();
      } while (dirty);
      return result;
    },
    close() { for (const state of slots) state.cleanup?.(); },
  };
}
function find(node, type) {
  if (!node || typeof node !== "object") return null;
  if (node.type === type) return node;
  return [node.props?.children].flat().map((child) => find(child, type)).find(Boolean) ?? null;
}
const running = (overrides = {}) => ({ id: "ing_one", index_id: "idx_one", revision: 5,
  state: "running", allowed_actions: ["pause"], ...overrides });
function host() {
  const requests = [];
  const command = (action) => (id, intent, signal) => {
    const request = { action, id, intent, signal, ...deferred() };
    requests.push(request);
    return request.promise;
  };
  return { requests, ingestion: { pause: command("pause"), resume: command("resume") } };
}

test("controls require host authority and an advertised action", () => {
  for (const run of [running(), running({ state: "paused", allowed_actions: ["resume"] })]) {
    assert.equal(controls(run, null).render(), null);
  }
  const { ingestion } = host();
  assert.equal(controls(null, ingestion).render(), null);
  assert.equal(controls(running({ state: "pausing", allowed_actions: [] }), ingestion).render(), null);
  assert.equal(find(controls(running({ state: "paused", allowed_actions: ["resume"] }), ingestion).render(), "button").props.children, "Resume import");
});

test("an uncertain reply retries the original operation, revision and index", async () => {
  const { ingestion, requests } = host();
  const view = controls(running(), ingestion);
  find(view.render(), "button").props.onClick();
  assert.equal(find(view.render(), "button").props.disabled, true);
  requests[0].reject(new Error("connection lost"));
  await microtasks();
  assert.equal(find(view.render(), "button").props.children, "Retry Pause import");
  find(view.render({ run: running({ revision: 6 }) }), "button").props.onClick();
  assert.deepEqual(requests[1].intent, requests[0].intent);
  assert.equal(requests[1].intent.expected_revision, 5);
  assert.equal(requests[1].intent.indexId, "idx_one");
  requests[1].reject(new ApiRequestError(409, "revision_conflict", "changed"));
  await microtasks();
  find(view.render({ run: running({ revision: 7 }) }), "button").props.onClick();
  assert.equal(requests[2].intent.expected_revision, 7);
  assert.notEqual(requests[2].intent.idempotencyKey, requests[0].intent.idempotencyKey);
  view.close();
});

test("observed pause settles an uncertain request without a late error replacing it", async () => {
  const { ingestion, requests } = host();
  const view = controls(running(), ingestion);
  find(view.render(), "button").props.onClick();
  const paused = running({ state: "paused", revision: 7, allowed_actions: ["resume"] });
  assert.equal(find(view.render({ run: paused }), "button").props.children, "Resume import");
  assert.equal(requests[0].signal.aborted, true);
  requests[0].reject(new Error("late timeout"));
  await microtasks();
  assert.equal(find(view.render(), "p"), null);
  view.close();
});

test("replacing the index or host retires pending intent and ignores its late reply", async () => {
  for (const replaceHost of [false, true]) {
    const first = host(), second = host();
    const view = controls(running(), first.ingestion);
    find(view.render(), "button").props.onClick();
    const next = replaceHost ? { ingestion: second.ingestion }
      : { run: running({ id: "ing_two", index_id: "idx_two" }) };
    assert.equal(find(view.render(next), "button").props.disabled, false);
    assert.equal(first.requests[0].signal.aborted, true);
    first.requests[0].reject(new Error("old connection timed out"));
    await microtasks();
    assert.equal(find(view.render(), "p"), null);
    find(view.render(), "button").props.onClick();
    const request = replaceHost ? second.requests[0] : first.requests[1];
    assert.equal(request.intent.indexId, replaceHost ? "idx_one" : "idx_two");
    assert.notEqual(request.intent.idempotencyKey, first.requests[0].intent.idempotencyKey);
    view.close();
    assert.equal(request.signal.aborted, true);
  }
});
