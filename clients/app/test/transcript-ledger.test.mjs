// The ledger's §04 vocabulary, its fold rules (T-3) and its hue rules (T-5).
// Fixtures are hand-built TimelineEntry objects: never a live home directory,
// never a captured transcript.
import { strict as assert } from "node:assert";
import { describe, it } from "node:test";
import {
  FOLD_NAME_LIMIT,
  RUN_FOLD_THRESHOLD,
  commandName,
  ledgerLine,
  ledgerLines,
  linkedChildSessionIds,
  objectText,
  runSummary,
} from "../renderer/src/lib/transcript/index.ts";

/* ── factories ───────────────────────────────────────────────────────────── */

let seq = 0;
const CTX = { cwd: "/w", home: "/Users/x" };

const inline = (text) => ({ type: "inline", text });
const file = (path) => ({ type: "file", path, resolved: true });
const directory = (path) => ({ type: "directory", path, resolved: true });
const url = (value) => ({ type: "url", url: value });

function entry(body, over = {}) {
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

function tool(action, over = {}) {
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

const line = (action, over = {}) => ledgerLine(tool(action, over), CTX);
const sentence = (l) => `${l.verb} ${objectText(l.object)}`;

const shell = (command, over = {}) =>
  tool(
    { type: "shell", command: inline(command), cwd: null, timeout_ms: null, background: false, outcome: over.outcome ?? null },
    over,
  );

const read = (path, over = {}) =>
  tool(
    {
      type: "file_read",
      targets: [{ resource: file(path), range: null, media_kind: null }],
      outcome: over.readOutcome ?? { truncated: false },
    },
    over,
  );

/* ── §04, family by family ───────────────────────────────────────────────── */

describe("§04 — the twenty-eight families", () => {
  it("file_read states the path with its range", () => {
    const l = line({
      type: "file_read",
      targets: [
        { resource: file("/w/src/CardShell.tsx"), range: { unit: "line", start: 1, count: 180 }, media_kind: null },
      ],
      outcome: { truncated: false },
    });
    assert.equal(l.verb, "Read");
    assert.equal(objectText(l.object), "src/CardShell.tsx");
    assert.deepEqual(
      l.object.map((p) => [p.text, p.tone]),
      [["src/", "dir"], ["CardShell.tsx", "plain"]],
    );
    assert.equal(l.result, "L1–180");
    assert.equal(l.glyph, "read");
    assert.equal(l.cls, "look");
    assert.equal(l.detail.kind, "none"); // reads keep no content by rule
  });

  it("file_read reports truncation and a media kind", () => {
    const l = line({
      type: "file_read",
      targets: [{ resource: file("/w/DESIGN.md"), range: null, media_kind: "image" }],
      outcome: { truncated: true },
    });
    assert.equal(l.result, "truncated · image");
  });

  it("file_list counts entries", () => {
    const l = line({
      type: "file_list",
      directory: directory("/w/src/design-system"),
      depth: null,
      filter: null,
      outcome: { entry_count: 24 },
    });
    assert.equal(sentence(l), "Listed src/design-system");
    assert.equal(l.result, "24 entries");
  });

  it("file_search states matches and files, and names its mode", () => {
    const l = line({
      type: "file_search",
      query: "CARD_RADIUS",
      mode: "content",
      roots: [],
      filters: [],
      outcome: { match_count: 14, file_count: 6, paths: [file("/w/src/CardShell.tsx")], total_paths: 6 },
    });
    assert.equal(sentence(l), "Searched CARD_RADIUS in files");
    assert.equal(l.object[0].tone, "query");
    assert.equal(l.object[1].tone, "aside");
    assert.equal(l.result, "14 matches · 6 files");
    assert.equal(l.detail.kind, "paths");
  });

  it("file_search says `found nothing` at an honest zero", () => {
    const l = line({
      type: "file_search",
      query: "gpu",
      mode: "regex",
      roots: [],
      filters: [],
      outcome: { match_count: 0, file_count: 0, paths: [], total_paths: 0 },
    });
    assert.equal(l.result, "found nothing");
    assert.equal(l.state, "done");
    assert.equal(l.resultTone, "plain");
  });

  it("file_search searches FOR files when the mode is a name or a glob", () => {
    const l = line({
      type: "file_search",
      query: "*.tsx",
      mode: "glob",
      roots: [],
      filters: [],
      outcome: null,
    });
    assert.equal(sentence(l), "Searched *.tsx for files");
    assert.equal(l.result, null);
  });

  it("code_lookup names its operation and counts locations", () => {
    const l = line({
      type: "code_lookup",
      operation: "references",
      query: "silhouette",
      path: null,
      outcome: {
        locations: [{ resource: file("/w/a.ts"), line: 3, column: null, summary: null }],
        total_count: 3,
      },
    });
    assert.equal(sentence(l), "Looked up silhouette references");
    assert.equal(l.result, "3 locations");
    assert.equal(l.detail.kind, "locations");
  });

  it("web_search counts sources and carries their hosts", () => {
    const l = line({
      type: "web_search",
      query: "webgpu vertex pulling",
      allowed_domains: [],
      blocked_domains: [],
      outcome: {
        sources: [{ title: "gpuweb/gpuweb", url: "https://github.com/gpuweb/gpuweb" }],
        total_count: 5,
      },
    });
    assert.equal(sentence(l), "Searched the web webgpu vertex pulling");
    assert.equal(l.result, "5 sources");
    assert.equal(l.detail.kind, "sources");
    assert.equal(l.detail.sources[0].host, "github.com");
  });

  it("web_fetch drops the scheme and keeps the title out of the result", () => {
    const l = line({
      type: "web_fetch",
      url: url("https://developer.mozilla.org/en-US/docs/Web/API/WebGPU_API"),
      purpose: null,
      outcome: {
        final_url: "https://developer.mozilla.org/en-US/docs/Web/API/WebGPU_API",
        title: "WebGPU API — Web APIs | MDN",
        http_status: 200,
      },
    });
    assert.equal(sentence(l), "Fetched developer.mozilla.org/en-US/docs/Web/API/WebGPU_API");
    assert.equal(l.result, "HTTP 200");
    assert.equal(l.detail.kind, "fetch");
    assert.equal(l.detail.fetch.title, "WebGPU API — Web APIs | MDN");
    assert.equal(l.detail.fetch.host, "developer.mozilla.org");
  });

  it("web_navigate opens a page or finds in one", () => {
    const opened = line({
      type: "web_navigate",
      operation: "open_page",
      url: url("https://developer.chrome.com/docs/web-platform/webgpu"),
      pattern: null,
      outcome: { final_url: null, title: null, http_status: 200 },
    });
    assert.equal(opened.verb, "Opened");
    assert.equal(opened.result, "HTTP 200");
    const found = line({
      type: "web_navigate",
      operation: "find_in_page",
      url: null,
      pattern: "vertex pulling",
      outcome: null,
    });
    assert.equal(found.verb, "Found in page");
  });

  it("tool_discovery states matches out of the whole registry", () => {
    const l = line({
      type: "tool_discovery",
      query: "figma",
      matches: ["a", "b", "c", "d"],
      total_count: 120,
    });
    assert.equal(sentence(l), "Searched tools figma");
    assert.equal(l.result, "4 of 120");
    assert.equal(l.detail.kind, "names");
  });

  it("file_change names the operation and states the diffstat", () => {
    const edited = line({
      type: "file_change",
      changes: [
        {
          operation: "update",
          path: file("/w/src/CardShell.tsx"),
          destination: null,
          diff: null,
          lines_added: 18,
          lines_removed: 4,
        },
      ],
    });
    assert.equal(sentence(edited), "Edited src/CardShell.tsx");
    assert.equal(edited.result, "+18 −4");
    assert.equal(edited.cls, "act");
    assert.equal(edited.detail.kind, "diff");
    assert.equal(edited.detail.diffstat.added, 18);
  });

  it("file_change never writes a zero it did not measure", () => {
    const created = line({
      type: "file_change",
      changes: [
        {
          operation: "create",
          path: file("/w/draft/index.html"),
          destination: null,
          diff: null,
          lines_added: 412,
          lines_removed: 0,
        },
      ],
    });
    assert.equal(created.verb, "Created");
    assert.equal(created.result, "+412");
  });

  it("file_change says `Changed` when the operation is unproven, and states a missing diff", () => {
    const l = line({
      type: "file_change",
      changes: [
        {
          operation: null,
          path: file("/w/draft/magnet.wgsl"),
          destination: null,
          diff: null,
          lines_added: null,
          lines_removed: null,
        },
      ],
    });
    assert.equal(sentence(l), "Changed draft/magnet.wgsl");
    assert.equal(l.result, "no diff recorded");
  });

  it("file_change points a move at its destination and folds many files into a count", () => {
    const moved = line({
      type: "file_change",
      changes: [
        {
          operation: "move",
          path: file("/w/a.ts"),
          destination: file("/w/b.ts"),
          diff: null,
          lines_added: null,
          lines_removed: null,
        },
      ],
    });
    assert.equal(sentence(moved), "Moved a.ts → b.ts");
    const many = line({
      type: "file_change",
      changes: [1, 2, 3].map((n) => ({
        operation: "update",
        path: file(`/w/f${n}.ts`),
        destination: null,
        diff: null,
        lines_added: 10,
        lines_removed: 4,
      })),
    });
    assert.equal(sentence(many), "Edited 3 files");
    assert.equal(many.result, "+30 −12");
  });

  it("shell is a LOOK, states nothing on success, and flags a background run", () => {
    const l = ledgerLine(shell("pnpm -F field-app typecheck"), CTX);
    assert.equal(sentence(l), "Ran pnpm -F field-app typecheck");
    assert.equal(l.result, null);
    assert.equal(l.cls, "look");
    assert.equal(l.detail.kind, "command");
    const background = line({
      type: "shell",
      command: inline("pnpm dev"),
      cwd: null,
      timeout_ms: null,
      background: true,
      outcome: null,
    });
    assert.equal(sentence(background), "Ran pnpm dev in background");
  });

  it("code_execute is an ACT and names its language", () => {
    const l = line({
      type: "code_execute",
      language: "python",
      code: inline("print(1)\nprint(2)"),
      cwd: null,
      outcome: null,
    });
    assert.equal(sentence(l), "Ran python print(1)");
    assert.equal(l.cls, "act");
    assert.equal(l.detail.kind, "code");
  });

  it("process names the operation and states the process state", () => {
    const l = line({
      type: "process",
      operation: "wait",
      process_id: "proc-1",
      input: null,
      outcome: { state: "exited" },
    });
    assert.equal(sentence(l), "Waited for proc-1");
    assert.equal(l.result, "exited");
  });

  it("orchestration counts children and mutes an unavailable trace", () => {
    const counted = line({
      type: "orchestration",
      runtime: "codex",
      trace: "complete",
      executed_children: ["a", "b", "c"],
      executed_child_count: 3,
    });
    assert.equal(sentence(counted), "Executed codex");
    assert.equal(counted.result, "3 children");
    const blind = line({
      type: "orchestration",
      runtime: "codex",
      trace: "unavailable",
      executed_children: [],
      executed_child_count: null,
    });
    assert.equal(blind.result, "trace unavailable");
    assert.equal(blind.resultTone, "muted");
  });

  it("delegate names the child and leaves its count to the component", () => {
    const row = tool({
      type: "delegate",
      agent_kind: "explore",
      name: "Explore",
      model: null,
      mode: null,
      objective: inline("find every consumer of CARD_RADIUS across the monorepo"),
      child_session_id: "child-1",
      resume_id: null,
      background: false,
      orphan_result_summary: null,
    });
    const l = ledgerLine(row, CTX);
    assert.equal(
      sentence(l),
      "Delegated Explore · find every consumer of CARD_RADIUS across the monorepo",
    );
    assert.equal(l.result, null); // `14 messages · 48s` needs the resolved child
    assert.deepEqual(l.childSessionIds, ["child-1"]);
    assert.deepEqual(linkedChildSessionIds(row), ["child-1"]);
    assert.equal(l.detail.kind, "delegate");
    assert.equal(l.detail.delegate.childSessionId, "child-1");
  });

  it("collaborate counts participants", () => {
    const l = line({
      type: "collaborate",
      operation: "send",
      recipient: "team-lead",
      team: null,
      summary: "the view model is ready",
      body: inline("full text"),
      linked_session_id: "peer-1",
      delivery_id: null,
      outcome: {
        participants: [{ name: "a", role: null, session_id: null, state: "active" }],
        total_count: 3,
      },
    });
    assert.equal(sentence(l), "Sent to team-lead · the view model is ready");
    assert.equal(l.result, "3 participants");
    assert.deepEqual(l.childSessionIds, ["peer-1"]);
    assert.equal(l.detail.kind, "participants");
  });

  it("question opens by default — it is conversation", () => {
    const l = line({
      type: "question",
      questions: [
        {
          id: "q1",
          prompt: inline("Which fold threshold should the ledger use?"),
          header: null,
          options: [{ id: "a", label: "8 lines", description: null }],
          allow_multiple: false,
        },
      ],
      answers: [{ question_id: "q1", values: [inline("8 lines")] }],
    });
    assert.equal(sentence(l), "Asked Which fold threshold should the ledger use?");
    assert.equal(l.object[0].tone, "aside");
    assert.equal(l.result, "answered");
    assert.equal(l.openByDefault, true);
    assert.equal(l.detail.kind, "questions");
  });

  it("skill, workflow and schedule state their name and their promise", () => {
    const skill = line({ type: "skill", name: "groundtruth", arguments: inline("--schemas-only") });
    assert.equal(sentence(skill), "Used skill groundtruth");
    assert.equal(skill.detail.kind, "text");

    const workflow = line({
      type: "workflow",
      name: "release",
      path: null,
      arguments: null,
      objective: inline("cut a tag"),
      resume_id: null,
      background: true,
    });
    assert.equal(sentence(workflow), "Ran workflow release");
    assert.equal(workflow.result, "in background");

    const schedule = line({
      type: "schedule",
      operation: "create",
      schedule_id: null,
      due_at: null,
      delay_ms: 20 * 60_000,
      reason: "check the deploy",
      objective: null,
    });
    assert.equal(sentence(schedule), "Scheduled a wake-up check the deploy");
    assert.equal(schedule.result, "in 20m");
  });

  it("artifact names its kind and counts its files", () => {
    const l = line({
      type: "artifact",
      operation: "create",
      artifact_kind: "document",
      name: "deck.pdf",
      description: null,
      resources: [file("/w/a.pdf"), file("/w/b.pdf")],
      media: null,
    });
    assert.equal(sentence(l), "Made an artifact deck.pdf · document");
    assert.equal(l.result, "2 files");
  });

  it("notification routes channel to destination", () => {
    const l = line({
      type: "notification",
      channel: "slack",
      destination: "#eng",
      summary: inline("the build is green"),
      delivery_id: null,
    });
    assert.equal(sentence(l), "Notified slack → #eng");
    assert.equal(l.result, null);
  });

  it("mode enters and leaves, and opens into nothing", () => {
    const entered = line({ type: "mode", operation: "enter", mode: "plan", workspace: null });
    assert.equal(sentence(entered), "Entered plan mode");
    assert.equal(entered.detail.kind, "none");
    const left = line({
      type: "mode",
      operation: "exit",
      mode: "worktree",
      workspace: directory("/w/tree"),
    });
    assert.equal(sentence(left), "Left worktree mode · tree");
  });

  it("task and plan lead with the operation", () => {
    const task = line({
      type: "task",
      operation: "create",
      task_id: "T-1",
      subject: "Ship the ledger",
      description: inline("all twenty-eight"),
      status: "in_progress",
      owner: null,
      blocker_ids: [],
    });
    assert.equal(sentence(task), "Task create Ship the ledger");
    assert.equal(task.result, "in progress");

    const plan = line({
      type: "plan",
      operation: "create",
      plan_id: null,
      title: "Rebuild the transcript",
      summary: null,
      step_count: 7,
    });
    assert.equal(sentence(plan), "Plan create Rebuild the transcript");
    assert.equal(plan.result, "7 steps");
  });

  it("finding reports the highest severity and opens by default", () => {
    const l = line({
      type: "finding",
      findings: [
        { severity: "low", title: "a", summary: null, resources: [] },
        { severity: "high", title: "b", summary: null, resources: [] },
      ],
      total_count: 4,
    });
    assert.equal(sentence(l), "Reported 4 findings");
    assert.equal(l.result, "high");
    assert.equal(l.openByDefault, true);
  });

  it("structured_output produces a titled document and opens by default", () => {
    const l = line({
      type: "structured_output",
      document: { type: "inline", title: "Audit", summary: null, blocks: [], resources: [] },
    });
    assert.equal(sentence(l), "Produced Audit");
    assert.equal(l.openByDefault, true);
    assert.equal(l.detail.kind, "document");
  });

  it("external leads with the provider and states its outcome field", () => {
    const l = line({
      type: "external",
      provider: "figma",
      operation: "search",
      subject: "Widget",
      fields: [{ role: "outcome", label: "result", value: { type: "text", value: "3 frames" } }],
      resources: [],
    });
    assert.equal(sentence(l), "figma search Widget");
    assert.equal(l.result, "3 frames");
    assert.equal(l.detail.kind, "fields");
  });

  it("unknown is honest: the native name and `no detail recorded`", () => {
    const l = line(
      { type: "unknown" },
      { native: { id: null, namespace: null, name: "Bash", provider: null } },
    );
    assert.equal(l.verb, "Bash");
    assert.equal(objectText(l.object), "no detail recorded");
    assert.equal(l.result, null);
    // Muted even though the row succeeded — the muted state is what italicises
    // the sentence, and this row never pretends a check.
    assert.equal(l.state, "muted");
    assert.equal(l.glyph, "help");
    assert.equal(l.detail.kind, "none");
  });

  it("unknown appends the status word when the row did not succeed", () => {
    const l = line(
      { type: "unknown" },
      { status: "unknown", native: { id: null, namespace: null, name: "exec", provider: null } },
    );
    assert.equal(l.verb, "exec");
    assert.equal(objectText(l.object), "no detail recorded · status unknown");
  });
});

/* ── T-5, the hue rules ──────────────────────────────────────────────────── */

describe("T-5 — hue only for what did not complete, or is still happening", () => {
  for (const status of ["failed", "denied", "timed_out"]) {
    it(`${status} is red`, () => {
      const l = ledgerLine(shell("cargo test", { status }), CTX);
      assert.equal(l.state, "failed");
      assert.equal(l.resultTone, "failed");
      assert.equal(l.result, status.replaceAll("_", " "));
    });
  }

  for (const status of ["cancelled", "interrupted", "unknown"]) {
    it(`${status} is muted, not red — nothing failed`, () => {
      const l = ledgerLine(shell("cargo test", { status }), CTX);
      assert.equal(l.state, "muted");
      assert.equal(l.resultTone, "muted");
    });
  }

  for (const status of ["running", "pending"]) {
    it(`${status} is the green breath; the component appends the elapsed`, () => {
      const l = ledgerLine(shell("cargo clippy", { status }), CTX);
      assert.equal(l.state, "running");
      assert.equal(l.resultTone, "running");
      assert.equal(l.result, "running");
    });
  }

  it("success is the absence of red", () => {
    const l = ledgerLine(shell("pnpm typecheck"), CTX);
    assert.equal(l.state, "done");
    assert.equal(l.resultTone, "plain");
  });

  it("a failed command states its exit code and the failure's first line", () => {
    const l = ledgerLine(
      shell("pnpm -F field-app test -- CardShell", {
        status: "failed",
        outcome: { exit_code: 1, signal: null, process_id: null },
        failure: { kind: "exit_non_zero", summary: "2 tests failed", diagnostic: null },
      }),
      CTX,
    );
    assert.equal(l.result, "exit 1 · 2 tests failed");
    assert.equal(l.resultTone, "failed");
  });

  it("a timeout states the budget in the seconds it was set in", () => {
    const l = line(
      {
        type: "shell",
        command: inline("cargo build"),
        cwd: null,
        timeout_ms: 120_000,
        background: false,
        outcome: null,
      },
      { status: "timed_out", failure: { kind: "timeout", summary: null, diagnostic: null } },
    );
    assert.equal(l.result, "timed out · 120s");
    assert.equal(l.state, "failed");
  });

  it("a source that REPORTS zero as a failure keeps its own words, in red", () => {
    // Grok says `found 0 matches` and calls it a failure. The line shows that,
    // because that is what the source said — the vocabulary's own `found
    // nothing` is for a source that is honest about a zero.
    const l = line(
      {
        type: "file_search",
        query: "gpu.*grid|grid.*gpu",
        mode: "regex",
        roots: [],
        filters: [],
        outcome: { match_count: 0, file_count: 0, paths: [], total_paths: 0 },
      },
      { status: "failed", failure: { kind: "unknown", summary: "found 0 matches", diagnostic: null } },
    );
    assert.equal(l.result, "found 0 matches");
    assert.equal(l.state, "failed");
    assert.equal(l.resultTone, "failed");
  });

  it("a failure with a long diagnostic opens by default; a one-phrase one does not", () => {
    const long = ledgerLine(
      shell("browser-use open http://localhost", {
        status: "failed",
        failure: {
          kind: "unknown",
          summary: null,
          diagnostic: "The browser-use CLI changed in 3.0.\nThe old preset subcommands are gone.",
        },
      }),
      CTX,
    );
    assert.equal(long.openByDefault, true);
    const short = ledgerLine(
      shell("node probe.mjs", {
        status: "failed",
        outcome: { exit_code: 1, signal: null, process_id: null },
        failure: { kind: "exit_non_zero", summary: "zero-length capsule", diagnostic: null },
      }),
      CTX,
    );
    assert.equal(short.result, "exit 1 · zero-length capsule");
    assert.equal(short.openByDefault, false);
  });

  it("a line with no result states its own span once it is a second long", () => {
    const l = ledgerLine(
      shell("pnpm -F field-app test", {
        timing: { started_at: 1_000, completed_at: 49_000, duration_ms: 48_000 },
      }),
      CTX,
    );
    assert.equal(l.result, "48s");
    assert.equal(l.durationMs, 48_000);
    assert.equal(l.at, 49_000); // `completed_at ?? row.at`
  });
});

/* ── T-3, the fold ───────────────────────────────────────────────────────── */

describe("T-3 — fold by class, not by run", () => {
  it("consecutive commands fold to one line and name themselves", () => {
    const lines = ledgerLines([shell("ls"), shell("cat README"), shell("grep -r x .")], CTX);
    assert.equal(lines.length, 1);
    const [fold] = lines;
    assert.equal(fold.folded, true);
    assert.equal(fold.foldCount, 3);
    assert.equal(fold.verb, "Ran");
    assert.equal(objectText(fold.object), "3 commands · ls, cat README, grep");
    assert.equal(fold.rows.length, 3);
  });

  it("a command's name is the program plus its first real argument", () => {
    assert.equal(commandName("pnpm -F field-app test"), "pnpm test");
    assert.equal(commandName("cargo clippy --all-targets -- -D warnings"), "cargo clippy");
    assert.equal(commandName("node scripts/probe-silhouette.mjs --bays 1"), "node probe-silhouette");
    assert.equal(commandName("git add ."), "git add");
    assert.equal(commandName("git commit -m 'x'"), "git commit");
    assert.equal(commandName("ls"), "ls");
    // Flag arity is unknowable, so a swallowed value that names nothing is dropped.
    assert.equal(commandName("grep -r x ."), "grep");
  });

  it("names are deduplicated in first-occurrence order", () => {
    const lines = ledgerLines([shell("git add ."), shell("git add -A"), shell("git push")], CTX);
    assert.equal(objectText(lines[0].object), "3 commands · git add, git push");
  });

  it("a failure BREAKS the fold and stands alone", () => {
    const lines = ledgerLines(
      [
        shell("pnpm typecheck"),
        shell("node probe.mjs", {
          status: "failed",
          outcome: { exit_code: 1, signal: null, process_id: null },
          failure: { kind: "exit_non_zero", summary: "zero-length capsule", diagnostic: null },
        }),
        shell("pnpm test"),
        shell("pnpm build"),
      ],
      CTX,
    );
    assert.equal(lines.length, 3);
    assert.equal(lines[0].folded, false);
    assert.equal(lines[0].verb, "Ran");
    assert.equal(lines[1].state, "failed");
    assert.equal(lines[1].folded, false);
    assert.equal(lines[2].folded, true);
    assert.equal(lines[2].foldCount, 2);
  });

  it("running and muted lines break the fold too", () => {
    const lines = ledgerLines(
      [shell("a"), shell("b", { status: "running" }), shell("c"), shell("d")],
      CTX,
    );
    assert.deepEqual(lines.map((l) => l.foldCount), [1, 1, 2]);
  });

  it("consecutive reads fold to `N files` and aggregate the truncations", () => {
    const lines = ledgerLines(
      [
        read("/w/src/CardShell.tsx"),
        read("/w/src/silhouette.ts"),
        read("/w/src/tokens.css"),
        read("/w/DESIGN.md", { readOutcome: { truncated: true } }),
      ],
      CTX,
    );
    assert.equal(lines.length, 1);
    assert.equal(lines[0].verb, "Read");
    assert.equal(
      objectText(lines[0].object),
      "4 files · CardShell.tsx, silhouette.ts, tokens.css, DESIGN.md",
    );
    assert.equal(lines[0].result, "1 truncated");
  });

  it("different LOOK families do not fold into each other", () => {
    const lines = ledgerLines([read("/w/a.ts"), read("/w/b.ts"), shell("ls"), shell("cat")], CTX);
    assert.deepEqual(lines.map((l) => l.family), ["file_read", "shell"]);
    assert.deepEqual(lines.map((l) => l.foldCount), [2, 2]);
  });

  it("ACTS never fold, even when their verb is also `Ran`", () => {
    const code = () =>
      tool({ type: "code_execute", language: "python", code: inline("print(1)"), cwd: null, outcome: null });
    const lines = ledgerLines([code(), code(), code()], CTX);
    assert.equal(lines.length, 3);
    assert.ok(lines.every((l) => !l.folded));
  });

  it("a fold of one is not a fold", () => {
    const lines = ledgerLines([shell("ls")], CTX);
    assert.equal(lines[0].folded, false);
    assert.equal(lines[0].foldCount, 1);
  });

  it(`past ${FOLD_NAME_LIMIT} names the fold elides and dates itself at the end`, () => {
    const commands = ["a", "b", "c", "d", "e", "f"].map((name, index) =>
      shell(name, { at: 1_000 + index * 1_000 }),
    );
    const [fold] = ledgerLines(commands, CTX);
    assert.equal(fold.elided, true);
    assert.equal(objectText(fold.object), "… 6 more commands");
    assert.equal(fold.at, 6_000); // the LAST stamp; the component draws the `→`
    assert.equal(fold.result, null); // the arrow states the span instead
  });

  it("a fold states its own span when it is not elided", () => {
    const [fold] = ledgerLines(
      [shell("pnpm typecheck", { at: 1_000 }), shell("node probe.mjs", { at: 22_000 })],
      CTX,
    );
    assert.equal(objectText(fold.object), "2 commands · pnpm typecheck, node probe");
    assert.equal(fold.result, "21s");
    assert.equal(fold.at, 1_000);
  });

  it("a search fold sums its matches and files", () => {
    const search = (query, matches, files) =>
      tool({
        type: "file_search",
        query,
        mode: "content",
        roots: [],
        filters: [],
        outcome: { match_count: matches, file_count: files, paths: [], total_paths: files },
      });
    const [fold] = ledgerLines([search("a", 10, 4), search("b", 4, 2)], CTX);
    assert.equal(objectText(fold.object), "2 searches · a, b");
    assert.equal(fold.result, "14 matches · 6 files");
  });
});

/* ── the run's closing sentence ──────────────────────────────────────────── */

describe("the run summary", () => {
  it("names NOUNS in first-occurrence order and counts rows, not lines", () => {
    const change = () =>
      tool({
        type: "file_change",
        changes: [
          {
            operation: "update",
            path: file("/w/a.ts"),
            destination: null,
            diff: null,
            lines_added: 1,
            lines_removed: 1,
          },
        ],
      });
    const rows = [
      read("/w/a.ts"),
      read("/w/b.ts"),
      change(),
      shell("ls"),
      shell("cat"),
      shell("grep"),
    ];
    const summary = runSummary(ledgerLines(rows, CTX));
    // Reads occurred first, then the edit, then the commands — §04's stated
    // rule is first occurrence, and it is the rule this follows.
    assert.deepEqual(summary.nouns, ["2 reads", "1 edit", "3 commands"]);
    assert.equal(summary.rowCount, 6);
    assert.equal(summary.lineCount, 3);
  });

  it("counts failures separately and measures the whole run's span", () => {
    const rows = [
      shell("a", { at: 1_000 }),
      shell("b", {
        at: 5_000,
        status: "failed",
        failure: { kind: "unknown", summary: "boom", diagnostic: null },
      }),
      shell("c", { at: 10_000 }),
    ];
    const summary = runSummary(ledgerLines(rows, CTX));
    assert.equal(summary.failed, 1);
    assert.equal(summary.spanMs, 9_000);
    assert.deepEqual(summary.nouns, ["3 commands"]);
  });

  it(`a run past ${RUN_FOLD_THRESHOLD} lines closes by default`, () => {
    const act = (n) =>
      tool({
        type: "file_change",
        changes: [
          {
            operation: "update",
            path: file(`/w/f${n}.ts`),
            destination: null,
            diff: null,
            lines_added: 1,
            lines_removed: 0,
          },
        ],
      });
    const eight = runSummary(ledgerLines([1, 2, 3, 4, 5, 6, 7, 8].map(act), CTX));
    assert.equal(eight.lineCount, 8);
    assert.equal(eight.closedByDefault, false);
    const nine = runSummary(ledgerLines([1, 2, 3, 4, 5, 6, 7, 8, 9].map(act), CTX));
    assert.equal(nine.lineCount, 9);
    assert.equal(nine.closedByDefault, true);
  });

  it("an empty run says nothing", () => {
    const summary = runSummary([]);
    assert.deepEqual(summary.nouns, []);
    assert.equal(summary.spanMs, null);
    assert.equal(summary.closedByDefault, false);
  });
});

/* ── paths ───────────────────────────────────────────────────────────────── */

describe("paths draw relative to where the reading happened", () => {
  it("shortens against the session's cwd, then home, then leaves it whole", () => {
    assert.equal(objectText(line({ type: "file_read", targets: [{ resource: file("/w/a/b.ts"), range: null, media_kind: null }], outcome: null }).object), "a/b.ts");
    assert.equal(
      objectText(
        ledgerLine(read("/Users/x/notes/todo.md"), { cwd: "/w", home: "/Users/x" }).object,
      ),
      "~/notes/todo.md",
    );
    assert.equal(
      objectText(ledgerLine(read("/elsewhere/z.ts"), { cwd: "/w", home: "/Users/x" }).object),
      "/elsewhere/z.ts",
    );
  });
});
