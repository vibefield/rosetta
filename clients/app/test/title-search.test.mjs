// Title search over the room's metadata — what the search sheet offers while
// the full-text index cannot answer yet (first import, resume, rebuild).
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  FULL_TEXT_PENDING,
  TITLE_MATCH_LIMIT,
  foldText,
  fullTextUnavailable,
  markTitle,
  matchSessionTitles,
  sessionTitleText,
  titleCountLabel,
  titleOptionId,
  titleTerms,
} from "../../room/src/lib/title-search.ts";

const now = new Date(2026, 9, 31, 12).getTime();
const day = 86_400_000;

function session(id, title, over = {}) {
  const at = "at" in over ? over.at : now - day;
  return {
    id,
    native_session_id: `${id}-native`,
    runtime_id: over.runtime_id ?? "rt-claude",
    project_id: "project_id" in over ? over.project_id : "p1",
    cwd: null,
    parent_session_id: over.parent ?? null,
    delegation: null,
    root_session_id: over.parent ?? id,
    role: over.parent ? "subagent" : "main",
    title,
    summary: over.summary ?? null,
    started_at: 1_000,
    updated_at: 2_000,
    activity: at === null ? null : { first_at: at - 60_000, last_at: at },
    stream_state: over.state ?? "pending",
  };
}

function project(id, name = id) {
  return { row: { id, name, root_paths: [], git_repo: null }, tint: `tint-${id}` };
}

const projects = new Map([
  ["p1", project("p1", "rosetta")],
  ["p2", project("p2", "mille")],
  ["__rosetta_unassigned__", project("__rosetta_unassigned__", "Unassigned sessions")],
]);
const kindOf = (runtimeId) => (runtimeId === "rt-codex" ? "codex" : "claude-code");
const everywhere = { project: null, agent: null, runtimeKind: kindOf };
const ids = (result) => result.matches.map((m) => m.session.id);
const text = (parts) => parts.map((p) => (p.mark ? `[${p.text}]` : p.text)).join("");

describe("fullTextUnavailable", () => {
  it("holds while the projection is unpublished or the index has no generation", () => {
    assert.equal(fullTextUnavailable("building", false), true);
    assert.equal(fullTextUnavailable("building", true), true);
    assert.equal(fullTextUnavailable("failed", false), true);
    assert.equal(fullTextUnavailable(null, false), true);
  });

  it("does not hold once full text can answer, even partially, or when nothing is known", () => {
    assert.equal(fullTextUnavailable("ready", true), false);
    assert.equal(fullTextUnavailable("catching-up", true), false);
    assert.equal(fullTextUnavailable("stale", true), false);
    // A failed index over a finished import keeps the ordinary sheet: the
    // pending line would not be true there.
    assert.equal(fullTextUnavailable("failed", true), false);
    assert.equal(fullTextUnavailable(null, null), false);
  });

  it("prints the promised line", () => {
    assert.equal(FULL_TEXT_PENDING, "Full-text search arrives when the import finishes.");
  });
});

describe("titleTerms and foldText", () => {
  it("splits words, keeps a quoted phrase whole, folds case and accents, drops duplicates", () => {
    assert.deepEqual(titleTerms("Cargo  TEST"), ["cargo", "test"]);
    assert.deepEqual(titleTerms('fix "cargo test" fix'), ["fix", "cargo test"]);
    assert.deepEqual(titleTerms("Café"), ["cafe"]);
    assert.deepEqual(titleTerms('"open'), ["open"]);
    assert.deepEqual(titleTerms('"" " '), []);
    assert.deepEqual(titleTerms("   "), []);
  });

  it("folds non-ASCII text code point by code point", () => {
    assert.equal(foldText("Ünïcödé ÉTÉ"), "unicode ete");
    assert.equal(foldText("修复 Bug"), "修复 bug");
    assert.equal(foldText("plain ascii"), "plain ascii");
  });

  it("names a session by its title, else its summary, else nothing", () => {
    assert.equal(sessionTitleText(session("s", "T", { summary: "S" })), "T");
    assert.equal(sessionTitleText(session("s", null, { summary: "S" })), "S");
    assert.equal(sessionTitleText(session("s", null)), null);
  });
});

describe("markTitle", () => {
  it("marks every occurrence of every term in the original spelling", () => {
    assert.equal(text(markTitle("Fix the Cargo test, then cargo build", ["cargo"])), "Fix the [Cargo] test, then [cargo] build");
    assert.equal(text(markTitle("Café crème", ["cafe", "creme"])), "[Café] [crème]");
  });

  it("merges overlapping and adjacent terms into one mark", () => {
    assert.equal(text(markTitle("searchsheet", ["search", "rchsh"])), "[searchsh]eet");
    assert.equal(text(markTitle("aaaa", ["aa"])), "[aaaa]");
  });

  it("keeps the whole title when nothing marks", () => {
    assert.deepEqual(markTitle("Nothing here", ["zzz"]), [{ text: "Nothing here", mark: false }]);
    assert.deepEqual(markTitle("", ["a"]), []);
  });
});

describe("matchSessionTitles", () => {
  const sessions = [
    session("old", "Refactor the search sheet", { at: now - 9 * day }),
    session("new", "Search sheet keyboard walk", { at: now - day }),
    session("unknown", "search sheet notes", { at: null }),
    session("other", "Ledger compaction", { at: now }),
    session("child", "search sheet subagent", { parent: "new", at: now }),
    session("summary", null, { summary: "Summary mentions the Search Sheet", at: now - 2 * day }),
    session("untitled", null, { at: now }),
    session("codex", "Search sheet in codex", { runtime_id: "rt-codex", project_id: "p2", at: now - 3 * day }),
    session("loose", "Search sheet without a project", { project_id: null, at: now - 4 * day }),
    session("hidden", "Search sheet in a project the room does not show", { project_id: "gone", at: now }),
  ];

  it("matches top-level threads whose title holds every term, newest activity first", () => {
    const result = matchSessionTitles("search SHEET", sessions, projects, everywhere);
    assert.deepEqual(ids(result), ["new", "summary", "codex", "loose", "old", "unknown"]);
    assert.equal(result.total, 6);
    const first = result.matches[0];
    assert.equal(first.id, titleOptionId("new"));
    assert.equal(first.index, 0);
    assert.equal(first.project.row.name, "rosetta");
    assert.equal(text(first.parts), "[Search] [sheet] keyboard walk");
    assert.deepEqual(result.matches.map((m) => m.index), [0, 1, 2, 3, 4, 5]);
  });

  it("requires every word, and a quoted phrase as written", () => {
    assert.deepEqual(ids(matchSessionTitles("sheet keyboard", sessions, projects, everywhere)), ["new"]);
    assert.deepEqual(ids(matchSessionTitles('"the search"', sessions, projects, everywhere)), ["summary", "old"]);
    assert.deepEqual(ids(matchSessionTitles('"search the"', sessions, projects, everywhere)), []);
    assert.deepEqual(ids(matchSessionTitles("sheet nowhere", sessions, projects, everywhere)), []);
  });

  it("narrows to the sheet's project and agent scope", () => {
    assert.deepEqual(
      ids(matchSessionTitles("search", sessions, projects, { ...everywhere, project: "p2" })),
      ["codex"],
    );
    assert.deepEqual(
      ids(matchSessionTitles("search", sessions, projects, { ...everywhere, agent: "codex" })),
      ["codex"],
    );
    assert.deepEqual(
      ids(matchSessionTitles("search", sessions, projects, { ...everywhere, project: "p1", agent: "claude-code" })),
      ["new", "summary", "old", "unknown"],
    );
  });

  it("answers nothing for an empty query", () => {
    assert.deepEqual(matchSessionTitles("  ", sessions, projects, everywhere), { matches: [], total: 0 });
  });

  it("caps the list and keeps the full count", () => {
    const many = Array.from({ length: TITLE_MATCH_LIMIT + 7 }, (_, i) =>
      session(`s${String(i).padStart(3, "0")}`, `import note ${i}`, { at: now - i * 60_000 }),
    );
    const result = matchSessionTitles("import", many, projects, everywhere);
    assert.equal(result.matches.length, TITLE_MATCH_LIMIT);
    assert.equal(result.total, TITLE_MATCH_LIMIT + 7);
    assert.equal(result.matches[0].session.id, "s000");
    assert.equal(titleCountLabel(result), `first ${TITLE_MATCH_LIMIT} of ${TITLE_MATCH_LIMIT + 7} titles`);
    assert.equal(matchSessionTitles("import", many, projects, everywhere, 3).matches.length, 3);
  });

  it("counts in the sheet's words", () => {
    assert.equal(titleCountLabel({ matches: [{}], total: 1 }), "1 title");
    assert.equal(titleCountLabel({ matches: [{}, {}], total: 2 }), "2 titles");
    assert.equal(titleCountLabel({ matches: [], total: 0 }), "0 titles");
  });
});
