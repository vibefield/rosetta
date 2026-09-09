// The search sheet's view model — what the sheet decides without a DOM.
import { strict as assert } from "node:assert";
import { describe, it } from "node:test";
import {
  buildSheetLines,
  cleanSnippet,
  countSentence,
  forgetQuery,
  hitWhen,
  hitsInReadingOrder,
  kindWord,
  moveCursor,
  moveSection,
  readRecentQueries,
  rememberQuery,
  sessionCrumb,
  stateSentence,
} from "../renderer/src/lib/search-sheet.ts";
import { createSearchResultsViewModel } from "../renderer/src/lib/search-results.ts";

/* ── factories ───────────────────────────────────────────────────────────── */

function session(id, over = {}) {
  return {
    id,
    native_session_id: `${id}-native-uuid-0000-0000`,
    runtime_id: "rt1",
    project_id: over.project_id ?? "p1",
    cwd: null,
    parent_session_id: over.parent_session_id ?? null,
    delegation: null,
    root_session_id: over.root_session_id ?? id,
    role: over.role ?? "main",
    title: "title" in over ? over.title : `Session ${id}`,
    summary: "summary" in over ? over.summary : null,
    started_at: 1_000,
    updated_at: over.updated_at ?? 2_000,
    stream_state: "indexed",
  };
}

function project(id, name = id) {
  return { row: { id, name, root_paths: [], git_repo: null }, tint: "oklch(0.6 0.15 200)" };
}

function hit(session_id, timeline_id, at, snippet = `[needle] ${timeline_id}`, kind = "user") {
  return { session_id, timeline_id, kind, at, snippet, score: -1 };
}

function memoryStore() {
  const map = new Map();
  return {
    getItem: (k) => (map.has(k) ? map.get(k) : null),
    setItem: (k, v) => map.set(k, v),
  };
}

/* ── the flat list ───────────────────────────────────────────────────────── */

describe("buildSheetLines", () => {
  it("draws one sticky head per session with its hits beneath in reading order", () => {
    const sessions = [session("s1"), session("s2", { project_id: "p2" })];
    const projects = new Map([["p1", project("p1", "rosetta")], ["p2", project("p2", "mille")]]);
    const hits = [
      hit("s1", "t-late", 3_000),
      hit("s1", "t-early", 1_000),
      hit("s1", "t-unknown", null),
      hit("s2", "t-solo", 2_000),
    ];
    const model = createSearchResultsViewModel(hits, sessions, projects);
    const { lines, options } = buildSheetLines(model);

    assert.equal(lines.length, 2 + 4);
    assert.deepEqual(
      lines.map((l) => l.type),
      ["head", "option", "option", "option", "head", "option"],
    );
    // s1 has more hits, so it leads; inside it time orders the walk and an
    // unreported time sinks to the end rather than masquerading as epoch zero.
    assert.equal(lines[0].session.id, "s1");
    assert.deepEqual(
      options.slice(0, 3).map((o) => o.hit.timeline_id),
      ["t-early", "t-late", "t-unknown"],
    );
    assert.deepEqual(options.map((o) => o.index), [0, 1, 2, 3]);
    assert.equal(lines[0].project.row.name, "rosetta");
    assert.equal(lines[4].hitCount, 1);
  });

  it("lands a child-session hit under its root thread but keeps the hit's own session", () => {
    const sessions = [session("root"), session("child", { parent_session_id: "root", root_session_id: "root", role: "subagent" })];
    const projects = new Map([["p1", project("p1")]]);
    const model = createSearchResultsViewModel([hit("child", "t1", 1)], sessions, projects);
    const { lines, options } = buildSheetLines(model);
    assert.equal(lines[0].session.id, "root");
    assert.equal(options[0].session.id, "root");
    assert.equal(options[0].hit.session_id, "child");
  });

  it("is empty for no model", () => {
    assert.deepEqual(buildSheetLines(null), { lines: [], options: [] });
  });
});

describe("hitsInReadingOrder", () => {
  it("orders by time then id and never mutates its input", () => {
    const input = [hit("s", "b", 2), hit("s", "a", 2), hit("s", "z", 1), hit("s", "q", null)];
    const out = hitsInReadingOrder(input);
    assert.deepEqual(out.map((h) => h.timeline_id), ["z", "a", "b", "q"]);
    assert.deepEqual(input.map((h) => h.timeline_id), ["b", "a", "z", "q"]);
  });
});

/* ── the cursor ──────────────────────────────────────────────────────────── */

describe("moveCursor", () => {
  it("walks without wrapping and clamps at both ends", () => {
    assert.equal(moveCursor(0, "ArrowUp", 5), 0);
    assert.equal(moveCursor(4, "ArrowDown", 5), 4);
    assert.equal(moveCursor(2, "ArrowDown", 5), 3);
    assert.equal(moveCursor(2, "ArrowUp", 5), 1);
    assert.equal(moveCursor(2, "Home", 5), 0);
    assert.equal(moveCursor(2, "End", 5), 4);
  });
  it("pages by eight from a resting cursor and from none", () => {
    assert.equal(moveCursor(-1, "ArrowDown", 20), 0);
    assert.equal(moveCursor(-1, "PageDown", 20), 8);
    assert.equal(moveCursor(15, "PageDown", 20), 19);
    assert.equal(moveCursor(15, "PageUp", 20), 7);
    assert.equal(moveCursor(3, "PageUp", 20), 0);
  });
  it("is -1 with nothing to walk and clamps a stale cursor to the new length", () => {
    assert.equal(moveCursor(3, "ArrowDown", 0), -1);
    assert.equal(moveCursor(9, "ArrowDown", 4), 3);
    assert.equal(moveCursor(9, "ArrowUp", 4), 2);
  });
});

describe("moveSection", () => {
  const opt = (sessionId) => ({ session: { id: sessionId } });
  const options = [opt("a"), opt("a"), opt("a"), opt("b"), opt("c"), opt("c")];
  it("jumps down to the next session's first row and clamps at the last row", () => {
    assert.equal(moveSection(0, 1, options), 3);
    assert.equal(moveSection(1, 1, options), 3);
    assert.equal(moveSection(3, 1, options), 4);
    assert.equal(moveSection(4, 1, options), 5);
    assert.equal(moveSection(5, 1, options), 5);
    assert.equal(moveSection(-1, 1, options), 3);
  });
  it("jumps up to this session's head, then to the previous session's head", () => {
    assert.equal(moveSection(2, -1, options), 0);
    assert.equal(moveSection(0, -1, options), 0);
    assert.equal(moveSection(5, -1, options), 4);
    assert.equal(moveSection(4, -1, options), 3);
    assert.equal(moveSection(3, -1, options), 0);
  });
  it("is -1 with nothing to walk", () => {
    assert.equal(moveSection(0, 1, []), -1);
  });
});

/* ── the words on a row ──────────────────────────────────────────────────── */

describe("cleanSnippet", () => {
  it("collapses whitespace and shortens home-rooted paths the way the ledger does", () => {
    const raw = "--- /Users/j/Projects/p015/src/lib/[debounce].ts\n+++ /Users/j/Projects/x";
    assert.equal(
      cleanSnippet(raw, "/Users/j"),
      "--- ~/Projects/p015/src/lib/[debounce].ts +++ ~/Projects/x",
    );
  });
  it("leaves a look-alike prefix and a missing home alone", () => {
    assert.equal(cleanSnippet("/Users/jane/x", "/Users/j"), "/Users/jane/x");
    assert.equal(cleanSnippet("  a   b  ", null), "a b");
    assert.equal(cleanSnippet("/Users/j/x", "/"), "/Users/j/x");
  });
});

describe("kindWord", () => {
  it("speaks the room's vocabulary", () => {
    assert.equal(kindWord("user", "Claude Code"), "you");
    assert.equal(kindWord("agent", "Claude Code"), "Claude Code");
    assert.equal(kindWord("tool_use", "Codex"), "activity");
    assert.equal(kindWord("collaboration", "Codex"), "team");
    assert.equal(kindWord("mystery", "Codex"), "mystery");
  });
});

describe("hitWhen", () => {
  const now = new Date(2026, 8, 8, 20, 0).getTime();
  it("prints the clock inside today and the day beyond it", () => {
    assert.equal(hitWhen(new Date(2026, 8, 8, 14, 22).getTime(), now), "14:22");
    assert.equal(hitWhen(new Date(2026, 8, 2, 9, 5).getTime(), now), "2 Sept · 09:05");
    assert.equal(hitWhen(new Date(2025, 3, 22, 9, 5).getTime(), now), "22 Apr 2025");
    assert.equal(hitWhen(null, now), "—");
  });
});

describe("sessionCrumb", () => {
  it("prefers title, then summary, then eight characters of the native id in mono", () => {
    assert.deepEqual(sessionCrumb(session("s", { title: "Fix search" })), { text: "Fix search", mono: false });
    assert.deepEqual(sessionCrumb(session("s", { title: null, summary: "a summary" })), { text: "a summary", mono: false });
    assert.deepEqual(sessionCrumb(session("df0c11ea", { title: null })), { text: "df0c11ea", mono: true });
  });
});

describe("countSentence / stateSentence", () => {
  it("counts with numbers, never the word results", () => {
    assert.equal(countSentence({ matches: 1, sessions: 1, projects: 1 }), "1 match · 1 session · 1 project");
    assert.equal(countSentence({ matches: 1284, sessions: 61, projects: 12 }), "1,284 matches · 61 sessions · 12 projects");
  });
  const base = { query: "x", error: null, readiness: "ready", progress: null, canonicalReady: true, ingestPaused: false, counts: { matches: 3, sessions: 2, projects: 1 }, limit: 200 };
  it("says what is true in the foot", () => {
    assert.equal(stateSentence(base), "3 matches · 2 sessions · 1 project");
    assert.equal(stateSentence({ ...base, counts: { matches: 200, sessions: 9, projects: 3 } }), "first 200 matches · narrow the search to see the rest");
    assert.equal(stateSentence({ ...base, error: "boom" }), "search failed · boom");
    assert.equal(stateSentence({ ...base, readiness: "building", progress: { documents_done: 8400, documents_total: 13700 } }), "indexing · 61% · 8,400 of 13,700 messages · results are partial");
    assert.equal(stateSentence({ ...base, readiness: "building", canonicalReady: false }), "history is being imported");
    assert.equal(stateSentence({ ...base, readiness: "failed" }), "search unavailable");
    assert.equal(stateSentence({ ...base, readiness: "catching-up" }), "search is catching up · results may be partial");
    assert.equal(stateSentence({ ...base, ingestPaused: true }), "transcript import paused · search covers what was read");
    assert.equal(stateSentence({ ...base, query: "" }), "");
  });
});

/* ── memory ──────────────────────────────────────────────────────────────── */

describe("recent queries", () => {
  it("remembers most recent first, folds duplicates, caps at eight, and forgets", () => {
    const store = memoryStore();
    assert.deepEqual(readRecentQueries(store), []);
    rememberQuery(store, "alpha");
    rememberQuery(store, "beta ");
    rememberQuery(store, "alpha");
    assert.deepEqual(readRecentQueries(store), ["alpha", "beta"]);
    for (let i = 0; i < 10; i += 1) rememberQuery(store, `q${i}`);
    assert.equal(readRecentQueries(store).length, 8);
    assert.equal(readRecentQueries(store)[0], "q9");
    forgetQuery(store, "q9");
    assert.equal(readRecentQueries(store)[0], "q8");
    assert.deepEqual(rememberQuery(store, "   "), readRecentQueries(store));
  });
  it("survives a refusing or corrupt store", () => {
    assert.deepEqual(readRecentQueries(null), []);
    assert.deepEqual(rememberQuery(null, "x"), ["x"]);
    const bad = { getItem: () => "{not json", setItem: () => { throw new Error("quota"); } };
    assert.deepEqual(readRecentQueries(bad), []);
    assert.deepEqual(rememberQuery(bad, "x"), ["x"]);
  });
});
