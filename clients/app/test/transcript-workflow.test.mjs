import { strict as assert } from "node:assert";
import { describe, it } from "node:test";
import {
  canonicalText,
  ledgerLine,
  linkedChildSessionIds,
  objectText,
  workflowAttemptIndex,
  workflowAttemptsFor,
  workflowRuntime,
  workflowWorkerState,
} from "../../room/src/lib/transcript/index.ts";

const inline = (text) => ({ type: "inline", text });
const file = (path) => ({ type: "file", path, resolved: true });

function worker(over = {}) {
  return {
    index: over.index ?? 1,
    key: over.key ?? "draft",
    label: over.label ?? "draft",
    phase_index: over.phase_index ?? 1,
    phase_title: over.phase_title ?? "Draft",
    state: over.state ?? "done",
    cached: over.cached ?? false,
    configured_model: "configured_model" in over ? over.configured_model : "claude-opus-5",
    configured_effort: "configured_effort" in over ? over.configured_effort : "max",
    observed_models: over.observed_models ?? [],
    observed_efforts: over.observed_efforts ?? [],
    child_session_id: over.child_session_id ?? "child-1",
    prompt_preview: over.prompt_preview ?? "Draft the API",
    result_preview: over.result_preview ?? "Drafted the API",
    last_tool_name: null,
    last_tool_summary: null,
    queued_at: 1_000,
    started_at: 1_100,
    last_progress_at: 4_900,
    duration_ms: over.duration_ms ?? 3_800,
    tokens: over.tokens ?? 42_000,
    tool_calls: over.tool_calls ?? 7,
    error: over.error ?? null,
  };
}

function run(over = {}) {
  const workers = over.workers ?? [worker()];
  return {
    run_id: over.run_id ?? "wf_run",
    task_id: over.task_id ?? "task-1",
    name: over.name ?? "api-design",
    summary: inline(over.summary ?? "Design the collaborative editor API"),
    status: over.status ?? "completed",
    started_at: 1_000,
    updated_at: 5_000,
    usage: {
      duration_ms: 4_000,
      tokens: 42_000,
      tool_calls: 7,
      agent_count: workers.length,
    },
    default_model: "claude-opus-5",
    arguments: null,
    script_path: file("/w/workflow.js"),
    script: null,
    phases: [{ index: 1, title: "Draft", detail: "write the first draft" }],
    workers,
    logs: ["drafted 1/1"],
    result_kind: "object",
    result: inline(JSON.stringify({ finalDoc: "/w/api.md", findingCount: 3 })),
    error: null,
    resources: [file("/w/api.md")],
    attempts: over.attempts ?? [],
    attempt_total_count: over.attempt_total_count ?? 0,
    journal: [],
    journal_total_count: 0,
    journal_started_count: 0,
    journal_result_count: 0,
  };
}

function action(over = {}) {
  const hydrated = "run" in over ? over.run : run(over.runOver);
  return {
    type: "workflow",
    name: over.name ?? "api-design",
    path: file("/w/workflow.js"),
    arguments: null,
    objective: inline("Design the collaborative editor API"),
    resume_id: over.resume_id ?? null,
    background: true,
    task_id: over.task_id ?? "task-1",
    run_id: over.run_id ?? hydrated?.run_id ?? null,
    attempt: over.attempt ?? 1,
    callback_summary: over.callback_summary ?? null,
    callback_result: over.callback_result ?? null,
    usage: over.usage ?? null,
    run: hydrated,
  };
}

let ordinal = 0;
function row(workflowAction, over = {}) {
  ordinal += 1;
  return {
    timeline_id: over.timeline_id ?? `workflow-${ordinal}`,
    session_id: "parent",
    ordinal,
    group: { id: `g-${ordinal}`, kind: "agent_cycle", derivation: "native" },
    response: null,
    block_index: null,
    native_id: null,
    parent_native_id: null,
    at: over.at ?? 1_000 * ordinal,
    model: null,
    effort: null,
    body: {
      type: "tool_use",
      tool: {
        correlation: { key: `tool-${ordinal}`, derivation: "native_id" },
        native: { id: `tool-${ordinal}`, namespace: null, name: "Workflow", provider: null },
        status: { value: over.status ?? "succeeded", basis: over.basis ?? "native_terminal_record" },
        action: workflowAction,
        failure: over.failure ?? null,
        timing: over.timing ?? null,
      },
    },
    parent_tool_native_id: null,
    usage: { input: null, output: null, cache_read: null, cache_write: null, thinking: null },
    provenance: { agent_version: null, src_cursor: null, src_offset: null },
  };
}

describe("Workflow ledger projection", () => {
  it("draws the hydrated run status, progress, objective, and file addresses", () => {
    const entry = row(action());
    const line = ledgerLine(entry, { cwd: "/w" });
    assert.equal(line.verb, "Ran workflow");
    assert.equal(objectText(line.object), "api-design · Design the collaborative editor API");
    assert.equal(line.result, "completed · 1/1");
    assert.equal(line.state, "done");
    assert.equal(line.openByDefault, true);
    assert.equal(line.detail.kind, "workflow");
    assert.equal(line.detail.workflow.objective, "Design the collaborative editor API");
    assert.deepEqual(line.detail.workflow.script.read, {
      sessionId: "parent",
      timelineId: entry.timeline_id,
      targetIndex: 0,
    });
    assert.equal(line.detail.workflow.resources[0].read.targetIndex, 1);
    assert.deepEqual(linkedChildSessionIds(entry), ["child-1"]);
  });

  it("names resumes and lets exact terminal run state override an accepted launch", () => {
    const resumed = ledgerLine(row(action({ attempt: 2, resume_id: "wf_run", run: run({ status: "killed" }) })));
    assert.equal(resumed.verb, "Resumed workflow");
    assert.match(objectText(resumed.object), /attempt 2, same run/);
    assert.equal(resumed.result, "killed · 1/1");
    assert.equal(resumed.state, "muted");
  });

  it("calls an immediate request failure did not start and invents no run", () => {
    const failed = ledgerLine(row(action({ run: null, run_id: null }), {
      status: "failed",
      failure: { kind: "execution", summary: "invalid option", diagnostic: "Unknown option" },
    }));
    assert.equal(failed.state, "failed");
    assert.equal(failed.result, "did not start · invalid option");
    assert.equal(failed.openByDefault, false);
  });
});

describe("Workflow attempts and worker evidence", () => {
  it("keeps resume attempts chronological while both point to one run", () => {
    const first = row(action({ run: null, run_id: "wf_run", attempt: 1 }), { at: 2_000, status: "succeeded" });
    const second = row(action({ run: null, run_id: "wf_run", attempt: 2, resume_id: "wf_run" }), {
      at: 4_000,
      status: "running",
    });
    const index = workflowAttemptIndex([second, first]);
    const attempts = workflowAttemptsFor(index, second);
    assert.deepEqual(attempts.map((attempt) => attempt.attempt), [1, 2]);
    assert.equal(attempts[1].resumed, true);
    assert.equal(attempts[1].status, "running");
  });

  it("uses server-hydrated attempts outside the loaded page and overlays live rows", () => {
    const history = [{
      timeline_id: "older-attempt",
      attempt: 1,
      task_id: "task-old",
      resumed: false,
      status: "succeeded",
      at: 1_000,
      completed_at: 2_000,
      duration_ms: 1_000,
      summary: "first attempt completed",
    }];
    const current = row(action({
      run: run({ attempts: history, attempt_total_count: 2 }),
      run_id: "wf_run",
      attempt: 2,
      resume_id: "wf_run",
    }), { timeline_id: "current-attempt", at: 4_000, status: "running" });
    const attempts = workflowAttemptsFor(workflowAttemptIndex([current]), current);
    assert.deepEqual(attempts.map((attempt) => attempt.timelineId), ["older-attempt", "current-attempt"]);
    assert.equal(attempts[1].status, "running");
  });

  it("prefers observed model and effort, then names configured and missing evidence", () => {
    assert.deepEqual(
      workflowRuntime(worker({ observed_models: ["claude-sonnet-5"], observed_efforts: ["high"] }), null),
      { basis: "observed", model: "claude-sonnet-5", effort: "high" },
    );
    assert.equal(workflowRuntime(worker(), null).basis, "configured");
    assert.deepEqual(
      workflowRuntime(worker({ configured_model: null, configured_effort: null }), null),
      { basis: "unknown", model: "unknown", effort: "unknown" },
    );
    assert.equal(
      workflowRuntime(worker({ observed_models: ["opus", "sonnet"] }), null).basis,
      "varies",
    );
  });

  it("preserves cached, interrupted, and never-started worker outcomes", () => {
    assert.equal(workflowWorkerState(worker({ cached: true }), "completed"), "reused");
    assert.equal(workflowWorkerState(worker({ state: "progress" }), "killed"), "interrupted");
    assert.equal(workflowWorkerState(worker({ state: "start" }), "cancelled"), "never started");
    assert.equal(workflowWorkerState(worker({ state: "error" }), "completed"), "error");
  });

  it("keeps canonical callback result text available without a sidecar", () => {
    const callback = action({
      run: null,
      callback_result: inline(JSON.stringify({ verdict: "usable" })),
    });
    assert.equal(canonicalText(callback.callback_result), '{"verdict":"usable"}');
  });
});
