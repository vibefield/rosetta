// The search sheet while an import runs: its meter only claims work that is
// moving, title matches stand in until full text can answer (and until the
// answer on screen is one it gave), real hits always win, and the cursor
// stays on its thread when the titles re-rank.
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import path from "node:path";
import { describe, it } from "node:test";
import { fileURLToPath } from "node:url";
import { SheetTitles } from "../../room/src/components/search/SearchSheet.tsx";
import { keptTitleCursor, sheetMeter, shownTitleMatches } from "../../room/src/lib/search-sheet.ts";
import {
  FULL_TEXT_PENDING,
  matchSessionTitles,
  titleOptionId,
  titlesStandIn,
} from "../../room/src/lib/title-search.ts";
import { dataHooks, microtasks } from "./support/hooks.mjs";
import { deferred, fakeClock } from "./support/protocol.mjs";

const ROOM = fileURLToPath(new URL("../../room/", import.meta.url));
const fromRoom = createRequire(path.join(ROOM, "package.json"));
const { createElement } = fromRoom("react");
const { renderToStaticMarkup } = fromRoom("react-dom/server");

const now = new Date(2026, 9, 31, 12).getTime();
const day = 86_400_000;

function session(id, title, at) {
  return {
    id,
    native_session_id: `${id}-native`,
    runtime_id: "rt-claude",
    project_id: "p1",
    cwd: null,
    parent_session_id: null,
    delegation: null,
    root_session_id: id,
    role: "main",
    title,
    summary: null,
    started_at: 1_000,
    updated_at: 2_000,
    activity: { first_at: at - 60_000, last_at: at },
    stream_state: "pending",
  };
}

const projects = new Map([
  ["p1", { row: { id: "p1", name: "rosetta", root_paths: [], git_repo: null }, tint: "tint-p1" }],
]);
const scope = { project: null, agent: null, runtimeKind: () => "claude-code" };
const sessions = [
  session("s1", "Fix the import meter", now - day),
  session("s2", "Import title search", now - 2 * day),
  session("s3", "Unrelated", now),
];

describe("sheetMeter", () => {
  const base = { readiness: "building", progress: null, canonicalReady: true, ingestPaused: false };

  it("sweeps for work under way with no amount, and fills from a real count", () => {
    assert.equal(sheetMeter(base), null);
    assert.equal(sheetMeter({ ...base, canonicalReady: false }), null);
    assert.equal(sheetMeter({ ...base, readiness: "catching-up" }), null);
    assert.equal(sheetMeter({ ...base, progress: { documents_done: 25, documents_total: 100 } }), 0.25);
    // A zero total is no amount.
    assert.equal(sheetMeter({ ...base, progress: { documents_done: 0, documents_total: 0 } }), null);
  });

  it("draws nothing for a failed index: nothing is running", () => {
    assert.equal(sheetMeter({ ...base, readiness: "failed" }), undefined);
    assert.equal(sheetMeter({ ...base, readiness: "failed", canonicalReady: null }), undefined);
    // Even a stray count does not fill it.
    assert.equal(
      sheetMeter({ ...base, readiness: "failed", progress: { documents_done: 1, documents_total: 2 } }),
      undefined,
    );
    // Before publication the import is what moves, as the foot says.
    assert.equal(sheetMeter({ ...base, readiness: "failed", canonicalReady: false }), null);
  });

  it("draws nothing while the import is paused, unless there is a real count to show", () => {
    assert.equal(sheetMeter({ ...base, ingestPaused: true }), undefined);
    assert.equal(sheetMeter({ ...base, readiness: "failed", canonicalReady: false, ingestPaused: true }), undefined);
    assert.equal(
      sheetMeter({ ...base, ingestPaused: true, progress: { documents_done: 1, documents_total: 4 } }),
      0.25,
    );
  });

  it("draws nothing for a ready index or an unknown one", () => {
    assert.equal(sheetMeter({ ...base, readiness: "ready" }), undefined);
    assert.equal(sheetMeter({ ...base, readiness: null }), undefined);
  });
});

describe("shownTitleMatches", () => {
  const titles = matchSessionTitles("import", sessions, projects, scope);

  it("lists the titles for a query when full text gave no hits", () => {
    assert.deepEqual(shownTitleMatches("import", titles, 0), titles.matches);
    assert.deepEqual(shownTitleMatches("import", { matches: [], total: 0 }, 0), []);
  });

  it("hides them when hits are present, for an empty query, or when the room offers none", () => {
    assert.equal(shownTitleMatches("import", titles, 3), null);
    assert.equal(shownTitleMatches("  ", titles, 0), null);
    assert.equal(shownTitleMatches("import", null, 0), null);
  });
});

describe("keptTitleCursor", () => {
  const titles = matchSessionTitles("import", sessions, projects, scope);
  const second = titleOptionId("s2");

  it("follows its thread through a re-rank under an unchanged query", () => {
    assert.equal(keptTitleCursor(titles.matches, { query: "import", id: second }, "import"), 1);
    const reranked = [...titles.matches].reverse();
    assert.equal(keptTitleCursor(reranked, { query: "import", id: second }, "import"), 0);
  });

  it("lets the first row take it for a new query, a vanished thread, or no list", () => {
    assert.equal(keptTitleCursor(titles.matches, { query: "imp", id: second }, "import"), -1);
    assert.equal(keptTitleCursor(titles.matches, { query: "import", id: titleOptionId("gone") }, "import"), -1);
    assert.equal(keptTitleCursor(titles.matches, { query: "import", id: null }, "import"), -1);
    assert.equal(keptTitleCursor(null, { query: "import", id: second }, "import"), -1);
  });
});

describe("titlesStandIn", () => {
  const answered = (readiness) => ({
    state: { readiness, progress: null, projection_epoch: null, revision: null },
    hits: [],
  });

  it("stands in while full text cannot answer", () => {
    assert.equal(titlesStandIn("building", false, null), true);
    assert.equal(titlesStandIn("building", true, answered("building")), true);
  });

  it("keeps standing in after the import finishes until full text has answered", () => {
    assert.equal(titlesStandIn("ready", true, answered("building")), true);
    assert.equal(titlesStandIn("ready", true, answered("ready")), false);
    assert.equal(titlesStandIn("ready", true, answered("catching-up")), false);
    assert.equal(titlesStandIn("ready", true, null), false);
  });
});

describe("SheetTitles", () => {
  const render = (titles, active = 0) =>
    renderToStaticMarkup(
      createElement(SheetTitles, {
        titles,
        query: "import",
        active,
        nowMs: now,
        getRuntimeKind: () => "claude-code",
        onHover() {},
        onOpen() {},
      }),
    );

  it("lists the matching titles under their head, then the pending line", () => {
    const markup = render(matchSessionTitles("import", sessions, projects, scope));
    assert.match(markup, /Titles that match/);
    assert.match(markup, /2 titles/);
    assert.equal(markup.match(/role="option"/g).length, 2);
    assert.match(markup, new RegExp(`id="${titleOptionId("s1")}" role="option" aria-selected="true"`));
    assert.match(markup, new RegExp(`id="${titleOptionId("s2")}" role="option" aria-selected="false"`));
    assert.match(markup, /<mark>import<\/mark>/);
    assert.match(markup, /<mark>Import<\/mark>/);
    assert.ok(markup.endsWith(`<p class="ah-sheet__pending" role="presentation">${FULL_TEXT_PENDING}</p>`));
  });

  it("says no title matches, and what is still to come, when none does", () => {
    const markup = render({ matches: [], total: 0 });
    assert.match(markup, /<strong>No titles match<\/strong>/);
    assert.match(markup, /<code>import<\/code>/);
    assert.ok(markup.includes(FULL_TEXT_PENDING));
    assert.doesNotMatch(markup, /role="option"/);
  });
});

describe("useSearch re-asking", () => {
  it("asks an unchanged query again when full text starts answering", async (t) => {
    const clock = fakeClock(t);
    const requests = [];
    const client = {
      search: (query, signal) => {
        const request = { query, signal, ...deferred() };
        requests.push(request);
        return request.promise;
      },
    };
    const hook = dataHooks(client)("useSearch", "import", {}, false);
    t.after(() => hook.close());
    hook.render();
    clock.tick(220);
    requests[0].resolve({ state: { readiness: "building" }, hits: [] });
    await microtasks();
    assert.equal(hook.render().response.state.readiness, "building");

    // Same query, same facets: only the key changed.
    hook.render("import", {}, true);
    assert.equal(hook.render().searching, true);
    // The building answer stays on screen while its successor is in flight.
    assert.equal(hook.render().response.state.readiness, "building");
    clock.tick(220);
    assert.equal(requests.length, 2);
    assert.equal(requests[1].query.q, "import");
    requests[1].resolve({ state: { readiness: "ready" }, hits: ["hit"] });
    await microtasks();
    const feed = hook.render();
    assert.deepEqual(feed.response.hits, ["hit"]);
    assert.equal(feed.searching, false);

    // An unchanged key asks nothing more.
    hook.render("import", {}, true);
    clock.tick(220);
    assert.equal(requests.length, 2);
  });
});
