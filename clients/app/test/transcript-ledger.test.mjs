// The ledger's §04 vocabulary, its fold rules (T-3) and its hue rules (T-5).
// Fixtures are hand-built TimelineEntry objects: never a live home directory,
// never a captured transcript.
import { strict as assert } from "node:assert";
import { describe, it } from "node:test";
import {
  COMMAND_PREVIEW_CHARACTER_LIMIT,
  FOLD_NAME_LIMIT,
  TASK_PREVIEW_LIMIT,
  commandDirectoryContext,
  commandPreviewText,
  commandName,
  ledgerLine,
  ledgerLines,
  linkedChildSessionIds,
  objectText,
  taskDisplayRows,
} from "../../room/src/lib/transcript/index.ts";

import {
  CTX,
  directory,
  entry,
  file,
  inline,
  line,
  read,
  sentence,
  shell,
  tool,
  url,
} from "./support/ledger-fixtures.mjs";

/* ── §04, family by family ───────────────────────────────────────────────── */

describe("§04 — the twenty-eight families", () => {
  it("file_read names the file and carries its range into the file detail", () => {
    const l = line({
      type: "file_read",
      targets: [
        { resource: file("/w/src/CardShell.tsx"), range: { unit: "line", start: 1, count: 180 }, media_kind: null },
      ],
      outcome: { truncated: false },
    });
    assert.equal(l.verb, "Read");
    assert.equal(objectText(l.object), "CardShell.tsx");
    assert.deepEqual(
      l.object.map((p) => [p.text, p.tone]),
      [["CardShell.tsx", "plain"]],
    );
    assert.equal(l.result, null);
    assert.equal(l.glyph, "read");
    assert.equal(l.cls, "look");
    assert.equal(l.detail.kind, "paths");
    assert.equal(l.detail.paths.length, 1);
    assert.equal(l.detail.paths[0].range, "L1–180");
    assert.deepEqual(l.detail.paths[0].read, {
      sessionId: "s1",
      timelineId: l.rows[0].timeline_id,
      targetIndex: 0,
    });
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

  it("file_change does not call a stored zero-line structural diff missing", () => {
    const l = line({
      type: "file_change",
      changes: [
        {
          operation: "move",
          path: file("/w/a.ts"),
          destination: file("/w/b.ts"),
          diff: inline("rename from a.ts\nrename to b.ts"),
          lines_added: null,
          lines_removed: null,
        },
      ],
    });
    assert.equal(l.result, null);
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
    const source = ledgerLine(
      shell("one\ntwo", {
        source_truncated: true,
        timeline_id: "source-command",
      }),
      CTX,
    );
    assert.deepEqual(
      {
        truncated: source.detail.commands[0].sourceTruncated,
        session: source.detail.commands[0].sessionId,
        timeline: source.detail.commands[0].timelineId,
      },
      { truncated: true, session: "s1", timeline: "source-command" },
    );
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

  it("renders one command line with a 42-character maximum including omission", () => {
    assert.equal(
      commandPreviewText("python3 - <<'PYEOF'\nimport sys", true),
      "python3 - <<'PYEOF'...",
    );
    const long = "é".repeat(50);
    const shown = commandPreviewText(long, true);
    assert.equal(Array.from(shown).length, COMMAND_PREVIEW_CHARACTER_LIMIT);
    assert.equal(shown, `${"é".repeat(39)}...`);
    assert.equal(
      commandPreviewText("x".repeat(COMMAND_PREVIEW_CHARACTER_LIMIT), false),
      "x".repeat(COMMAND_PREVIEW_CHARACTER_LIMIT),
    );
    assert.equal(shown.includes("\n"), false);
  });

  it("keeps every known cwd as one quiet command-frame badge", () => {
    const commandAt = (cwd) =>
      line({
        type: "shell",
        command: inline("pwd"),
        source_truncated: false,
        cwd: directory(cwd),
        timeout_ms: null,
        background: false,
        outcome: null,
      }).detail.commands[0];
    const root = commandAt("/w");
    const nested = commandAt("/w/draft/card-redesign");
    assert.equal(root.cwdIsSessionRoot, true);
    assert.equal(nested.cwdIsSessionRoot, false);
    assert.deepEqual(commandDirectoryContext([root]), {
      label: "w",
      title: "Working directory: w",
    });
    assert.deepEqual(commandDirectoryContext([nested]), {
      label: "draft/card-redesign",
      title: "Working directory: draft/card-redesign",
    });
    assert.deepEqual(commandDirectoryContext([root, nested]), {
      label: "2 folders",
      title: "Working directories: w, draft/card-redesign",
    });
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

  it("names a Codex exec batch by the inner tools instead of its transport wrapper", () => {
    const commands = line({
      type: "orchestration",
      runtime: "functions.exec",
      trace: "partial",
      executed_children: ["exec_command", "exec_command", "exec_command"],
      executed_child_count: null,
    });
    assert.equal(sentence(commands), "Ran commands");
    assert.equal(commands.result, null);

    const patches = line({
      type: "orchestration",
      runtime: "functions.exec",
      trace: "partial",
      executed_children: ["apply_patch", "apply_patch"],
      executed_child_count: null,
    });
    assert.equal(sentence(patches), "Applied patches");

    const mixed = line({
      type: "orchestration",
      runtime: "functions.exec",
      trace: "partial",
      executed_children: ["update_plan", "exec_command"],
      executed_child_count: null,
    });
    assert.equal(sentence(mixed), "Ran tool batch");
    assert.equal(mixed.result, "plan · command");

    const opaque = line({
      type: "orchestration",
      runtime: "functions.exec",
      trace: "unavailable",
      executed_children: [],
      executed_child_count: null,
    });
    assert.equal(sentence(opaque), "Ran tool batch");
    assert.equal(opaque.result, "trace unavailable");
    assert.equal(opaque.resultTone, "muted");
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

  it("renders a shutdown request distinctly and keeps its body", () => {
    const l = line({
      type: "collaborate",
      operation: "request_stop",
      recipient: "researcher",
      team: null,
      summary: null,
      body: inline("The assigned work is complete."),
      linked_session_id: null,
      delivery_id: null,
      outcome: null,
    });
    assert.equal(sentence(l), "Asked to stop researcher · The assigned work is complete.");
    assert.equal(l.detail.kind, "text");
    assert.equal(l.detail.text, "The assigned work is complete.");
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
    assert.equal(sentence(workflow), "Ran workflow release · cut a tag");
    assert.equal(workflow.result, "in background");
    assert.equal(workflow.detail.kind, "workflow");

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
      description: inline("Two exported slides"),
      resources: [file("/w/a.pdf"), file("/w/b.pdf")],
      media: null,
    });
    assert.equal(sentence(l), "Made an artifact deck.pdf · document");
    assert.equal(l.result, "2 files");
    assert.equal(l.detail.kind, "paths");
    assert.equal(l.detail.description, "Two exported slides");
    assert.deepEqual(
      l.detail.paths.map((path) => path.read),
      [
        { sessionId: "s1", timelineId: l.rows[0].timeline_id, targetIndex: 0 },
        { sessionId: "s1", timelineId: l.rows[0].timeline_id, targetIndex: 1 },
      ],
    );
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

  it("task actions name the delta and open to their point-in-time board", () => {
    const task = line({
      type: "task",
      operation: "create",
      task_id: "T-1",
      subject: "Ship the ledger",
      description: inline("all twenty-eight"),
      status: "pending",
      owner: null,
      blocker_ids: [],
      snapshot: {
        tasks: [
          {
            task_id: "T-1",
            subject: "Ship the ledger",
            status: "pending",
            owner: null,
            blocker_ids: [],
          },
        ],
        total_count: 1,
      },
    });
    assert.equal(sentence(task), "Added Ship the ledger");
    assert.equal(task.result, "0 of 1 done");
    assert.equal(task.openByDefault, true);
    assert.equal(task.detail.kind, "tasks");
    assert.equal(task.detail.change, "added");

    const completed = line({
      type: "task",
      operation: "update",
      task_id: "T-1",
      subject: "Ship the ledger",
      description: null,
      status: "completed",
      owner: null,
      blocker_ids: [],
      snapshot: {
        tasks: [
          {
            task_id: "T-1",
            subject: "Ship the ledger",
            status: "completed",
            owner: null,
            blocker_ids: [],
          },
        ],
        total_count: 1,
      },
    });
    assert.equal(sentence(completed), "Completed Ship the ledger");
    assert.equal(completed.result, "1 of 1 done");
    assert.equal(completed.detail.change, "done");

    const board = ["1", "2", "3", "4", "5"].map((task_id) => ({
      task_id,
      subject: `Task ${task_id}`,
      status: task_id === "1" ? "completed" : "pending",
      owner: null,
      blocker_ids: [],
    }));
    assert.equal(TASK_PREVIEW_LIMIT, 3);
    assert.deepEqual(
      taskDisplayRows(board, "4", false).map((item) => item.task_id),
      ["2", "3", "4"],
      "the current task follows its two nearest preceding peers",
    );
    assert.deepEqual(
      taskDisplayRows(board, "4", true).map((item) => item.task_id),
      ["1", "2", "3", "5", "4"],
      "full disclosure keeps every task and still pins the current one last",
    );

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

  it("a terminal record without an outcome is quiet completed activity", () => {
    const l = ledgerLine(
      shell("git status", {
        status: "unknown",
        basis: "native_terminal_record",
      }),
      CTX,
    );
    assert.equal(l.state, "done");
    assert.equal(l.result, null);
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

  it("consecutive reads keep their names and ranges while aggregating truncations", () => {
    const lines = ledgerLines(
      [
        read("/w/src/CardShell.tsx", { range: { unit: "line", start: 1, count: 180 } }),
        read("/w/src/silhouette.ts", { range: { unit: "line", start: 40, count: 12 } }),
        read("/w/src/tokens.css"),
        read("/w/DESIGN.md", { readOutcome: { truncated: true } }),
      ],
      CTX,
    );
    assert.equal(lines.length, 1);
    assert.equal(lines[0].verb, "Read");
    assert.equal(
      objectText(lines[0].object),
      "CardShell.tsx, silhouette.ts, tokens.css, DESIGN.md",
    );
    assert.equal(lines[0].result, "1 truncated");
    assert.equal(lines[0].detail.kind, "paths");
    assert.deepEqual(
      lines[0].detail.paths.map((path) => path.name),
      ["CardShell.tsx", "silhouette.ts", "tokens.css", "DESIGN.md"],
    );
    assert.deepEqual(
      lines[0].detail.paths.map((path) => path.range ?? null),
      ["L1–180", "L40–51", null, null],
    );
    assert.ok(lines[0].detail.paths.every((path) => path.read?.sessionId === "s1"));
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

/* ── paths ───────────────────────────────────────────────────────────────── */

describe("paths draw relative to where the reading happened", () => {
  it("shows read basenames while retaining contextual paths in their details", () => {
    const cwdRead = line({ type: "file_read", targets: [{ resource: file("/w/a/b.ts"), range: null, media_kind: null }], outcome: null });
    assert.equal(objectText(cwdRead.object), "b.ts");
    assert.equal(cwdRead.detail.paths[0].label, "a/b.ts");
    assert.equal(
      objectText(
        ledgerLine(read("/Users/x/notes/todo.md"), { cwd: "/w", home: "/Users/x" }).object,
      ),
      "todo.md",
    );
    assert.equal(
      objectText(ledgerLine(read("/elsewhere/z.ts"), { cwd: "/w", home: "/Users/x" }).object),
      "z.ts",
    );
  });
});
