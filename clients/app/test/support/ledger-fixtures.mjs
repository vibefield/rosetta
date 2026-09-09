// Hand-built TimelineEntry factories for the ledger's tests — never a live
// home directory, never a captured transcript. Shared by the family, hue and
// fold tests and by the run-summary tests, which are one unit each.
import { ledgerLine, objectText } from "../../renderer/src/lib/transcript/index.ts";

let seq = 0;
export const CTX = { cwd: "/w", home: "/Users/x" };

export const inline = (text) => ({ type: "inline", text });
export const file = (path) => ({ type: "file", path, resolved: true });
export const directory = (path) => ({ type: "directory", path, resolved: true });
export const url = (value) => ({ type: "url", url: value });

export function entry(body, over = {}) {
  seq += 1;
  return {
    timeline_id: over.timeline_id ?? `t${seq}`,
    session_id: "s1",
    ordinal: over.ordinal ?? seq,
    group: over.group ?? { id: "g1", kind: "agent_cycle", derivation: "native" },
    response: over.response ?? null,
    block_index: null,
    native_id: null,
    parent_native_id: null,
    at: "at" in over ? over.at : 1_700_000_000_000,
    model: over.model ?? null,
    effort: over.effort ?? null,
    body,
    parent_tool_native_id: null,
    usage: over.usage ?? {
      input: null,
      output: null,
      cache_read: null,
      cache_write: null,
      thinking: null,
    },
    provenance: { agent_version: null, src_cursor: null, src_offset: null },
  };
}

export function tool(action, over = {}) {
  return entry(
    {
      type: "tool_use",
      tool: {
        correlation: { key: `k${seq}`, derivation: "native_id" },
        native: over.native ?? { id: null, namespace: null, name: "Tool", provider: null },
        status: { value: over.status ?? "succeeded", basis: over.basis ?? "native_state" },
        action,
        failure: over.failure ?? null,
        timing: over.timing ?? null,
      },
    },
    over,
  );
}

export const line = (action, over = {}) => ledgerLine(tool(action, over), CTX);
export const sentence = (l) => `${l.verb} ${objectText(l.object)}`;

export const shell = (command, over = {}) =>
  tool(
    {
      type: "shell",
      command: inline(command),
      source_truncated: over.source_truncated ?? false,
      cwd: null,
      timeout_ms: null,
      background: false,
      outcome: over.outcome ?? null,
    },
    over,
  );

export const read = (path, over = {}) =>
  tool(
    {
      type: "file_read",
      targets: [{ resource: file(path), range: over.range ?? null, media_kind: null }],
      outcome: over.readOutcome ?? { truncated: false },
    },
    over,
  );

