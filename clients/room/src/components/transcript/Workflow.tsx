import { useEffect, useState } from "react";
import type { ReactNode } from "react";
import type { CanonicalText, WorkflowRun, WorkflowWorker } from "@vibefield/rosetta-protocol";
import { AgentMark } from "../../lib/agents";
import { clock, tokenText } from "../../lib/format";
import { useReadingSession, useRoom } from "../../room-context";
import type {
  PathFact,
  WorkflowAttemptFact,
  WorkflowFact,
  WorkflowRuntimeFact,
} from "../../lib/transcript";
import {
  canonicalText,
  firstLine,
  workflowRuntime,
  workflowWorkerState,
} from "../../lib/transcript";
import { LineChevron } from "./glyphs";
import { PathList } from "./detail-parts";

export interface WorkflowChildThread {
  readonly childSessionId: string;
  readonly node: ReactNode;
}

interface PhaseGroup {
  readonly index: number;
  readonly title: string;
  readonly detail: string | null;
  readonly workers: readonly WorkflowWorker[];
}

function elapsed(ms: number): string {
  const seconds = Math.max(0, Math.round(ms / 1000));
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  const rest = seconds % 60;
  if (minutes < 60) return rest === 0 ? `${minutes}m` : `${minutes}m ${rest}s`;
  const hours = Math.floor(minutes / 60);
  const mins = minutes % 60;
  return mins === 0 ? `${hours}h` : `${hours}h ${String(mins).padStart(2, "0")}m`;
}

function quantity(value: number, one: string, many = `${one}s`): string {
  return `${value} ${value === 1 ? one : many}`;
}

function workerUsage(worker: WorkflowWorker): string | null {
  if (worker.cached) return "cached";
  const parts = [
    worker.duration_ms === null ? null : elapsed(worker.duration_ms),
    worker.tokens === null ? null : tokenText(worker.tokens),
    worker.tool_calls === null ? null : quantity(worker.tool_calls, "tool"),
  ].filter((part): part is string => part !== null);
  return parts.length === 0 ? null : parts.join(" · ");
}

function friendlyModel(model: string): string {
  const match = /^claude-(opus|sonnet|haiku)-(\d+)(?:[-.](\d+))?(?:-|$)/i.exec(model);
  if (match === null) return model;
  const family = match[1]![0]!.toUpperCase() + match[1]!.slice(1).toLowerCase();
  return `${family} ${match[2]}${match[3] === undefined ? "" : `.${match[3]}`}`;
}

function RuntimeLine({ runtime }: { readonly runtime: WorkflowRuntimeFact }) {
  return (
    <span className="wf-worker__runtime">
      <span className="wf-worker__basis">{runtime.basis}</span>{" "}
      <b>{friendlyModel(runtime.model)}</b>{" "}
      <span>· {runtime.effort === "unknown" ? "effort unknown" : `${runtime.effort} effort`}</span>
    </span>
  );
}

function workerDescription(worker: WorkflowWorker): string | null {
  return worker.result_preview ?? worker.last_tool_summary ?? worker.prompt_preview;
}

function workerDescriptionLine(description: string): string {
  return firstLine(description.replace(/^#{1,6}\s+/, ""), 72);
}

function WorkerRow({
  worker,
  run,
  child,
}: {
  readonly worker: WorkflowWorker;
  readonly run: WorkflowRun;
  readonly child: ReactNode | null;
}) {
  const [open, setOpen] = useState(false);
  const runtime = workflowRuntime(worker, run.default_model);
  const state = workflowWorkerState(worker, run.status);
  const description = workerDescription(worker);
  const expandable = child !== null || description !== null || worker.error !== null;
  const visualState = state === "error" ? "failed" : state === "running" ? "running" : undefined;
  const body = worker.error ?? description;
  const summary = (
    <>
      <span className="tr-line__g tm-avatar" aria-hidden="true">
        <AgentMark kind="claude-code" />
      </span>
      <span className="tr-line__v" title={worker.label}>{worker.label}</span>
      <span className="tr-line__o" title={description ?? undefined}>
        {description === null ? "no summary recorded" : workerDescriptionLine(description)}
      </span>
      <span className="tr-line__r">
        <span className={visualState === "failed" || visualState === "running" ? "why" : undefined}>
          {state}
        </span>
        {expandable && <span className="chev"><LineChevron /></span>}
      </span>
      <RuntimeLine runtime={runtime} />
      {workerUsage(worker) !== null && <span className="wf-worker__usage">{workerUsage(worker)}</span>}
    </>
  );

  if (!expandable) {
    return <div className="tr-line wf-worker" data-state={visualState}>{summary}</div>;
  }
  return (
    <details
      className="wf-worker-thread"
      open={open}
      onToggle={(event) => setOpen(event.currentTarget.open)}
    >
      <summary className="tr-line wf-worker" data-state={visualState}>{summary}</summary>
      <div className="wf-worker__detail">
        {body !== null && (
          <div className="wf-worker__result" data-state={worker.error === null ? undefined : "failed"}>
            <p>{body}</p>
          </div>
        )}
        {open && child}
      </div>
    </details>
  );
}

function phaseGroups(run: WorkflowRun): PhaseGroup[] {
  const known = new Set<number>();
  const groups: PhaseGroup[] = run.phases.map((phase) => {
    known.add(phase.index);
    return {
      index: phase.index,
      title: phase.title,
      detail: phase.detail,
      workers: run.workers.filter((worker) => worker.phase_index === phase.index),
    };
  });
  const orphaned = run.workers.filter(
    (worker) => worker.phase_index === null || !known.has(worker.phase_index),
  );
  if (orphaned.length > 0) {
    groups.push({
      index: groups.length === 0 ? 1 : Math.max(...groups.map((group) => group.index)) + 1,
      title: "Other workers",
      detail: "phase was not recorded",
      workers: orphaned,
    });
  }
  return groups;
}

function Phase({
  phase,
  run,
  current,
  children,
}: {
  readonly phase: PhaseGroup;
  readonly run: WorkflowRun;
  readonly current: boolean;
  readonly children: ReadonlyMap<string, ReactNode>;
}) {
  const [open, setOpen] = useState(current);
  const done = phase.workers.filter((worker) => worker.state === "done").length;
  const errors = phase.workers.filter((worker) => worker.state === "error").length;
  const count = `${done}/${phase.workers.length}${errors === 0 ? "" : ` · ${errors} error${errors === 1 ? "" : "s"}`}`;
  return (
    <details className="wf-phase" open={open} onToggle={(event) => setOpen(event.currentTarget.open)}>
      <summary className="wf-phase__head">
        <span className="wf-phase__index">{phase.index}</span>
        <span className="wf-phase__name">
          <strong>{phase.title}</strong>
          {phase.detail !== null && <span title={phase.detail}>{phase.detail}</span>}
        </span>
        <span className="wf-phase__count">{count}</span>
        <span className="chev"><LineChevron /></span>
      </summary>
      <div className="wf-phase__workers">
        {phase.workers.length === 0 ? (
          <p className="wf-empty">No workers recorded for this phase</p>
        ) : phase.workers.map((worker) => (
          <WorkerRow
            key={`${worker.index}-${worker.key ?? worker.label}`}
            worker={worker}
            run={run}
            child={worker.child_session_id === null ? null : (children.get(worker.child_session_id) ?? null)}
          />
        ))}
      </div>
    </details>
  );
}

function attemptOutcome(attempt: WorkflowAttemptFact, latest: boolean, run: WorkflowRun | null): string {
  if (latest && run !== null && run.status !== "unknown") return run.status;
  switch (attempt.status) {
    case "succeeded": return "completed";
    case "pending":
    case "running": return "running";
    case "timed_out": return "timed out";
    case "cancelled": return "cancelled";
    case "interrupted": return "interrupted";
    case "denied": return "denied";
    case "failed": return "failed";
    case "unknown": return "status unknown";
  }
}

function Attempts({
  attempts,
  run,
}: {
  readonly attempts: readonly WorkflowAttemptFact[];
  readonly run: WorkflowRun | null;
}) {
  if (attempts.length === 0) return null;
  const total = Math.max(run?.attempt_total_count ?? 0, attempts.length);
  const omitted = total - attempts.length;
  return (
    <section className="wf-attempts" aria-label="Workflow attempts">
      <span className="wf-section-label">Attempts</span>
      {omitted > 0 && (
        <p className="wf-empty">{quantity(omitted, "earlier attempt")} omitted from this view</p>
      )}
      {attempts.map((attempt, at) => {
        const outcome = attemptOutcome(attempt, at === attempts.length - 1, run);
        const state = ["failed", "denied", "timed out"].includes(outcome)
          ? "failed"
          : outcome === "running" ? "running" : undefined;
        const measured = attempt.durationMs ?? (at === attempts.length - 1 ? run?.usage.duration_ms ?? null : null);
        return (
          <div className="wf-attempt" data-state={state} key={attempt.timelineId} title={attempt.taskId ?? undefined}>
            <i className="wf-attempt__dot" aria-hidden="true" />
            <strong>Attempt {attempt.attempt}</strong>
            <span className="wf-attempt__launch">
              {attempt.resumed ? "resume" : "async launch"}
              {attempt.at === null ? "" : ` · ${clock(attempt.at)}`}
            </span>
            <span className="wf-attempt__outcome">
              {outcome}{measured === null ? "" : ` · ${elapsed(measured)}`}
            </span>
          </div>
        );
      })}
    </section>
  );
}

interface ResultField {
  readonly label: string;
  readonly value: string;
}

function fieldLabel(key: string): string {
  return key.replace(/([a-z0-9])([A-Z])/g, "$1 $2").replaceAll("_", " ").toLowerCase();
}

function resultFields(text: string): ResultField[] {
  try {
    const value: unknown = JSON.parse(text);
    if (value === null || Array.isArray(value) || typeof value !== "object") return [];
    return Object.entries(value).slice(0, 16).map(([key, item]) => ({
      label: fieldLabel(key),
      value: typeof item === "string" ? item : JSON.stringify(item),
    }));
  } catch {
    return [];
  }
}

function FullResult({ canonical }: { readonly canonical: CanonicalText }) {
  const [open, setOpen] = useState(false);
  const [loaded, setLoaded] = useState<{
    readonly digest: string;
    readonly text: string | null;
    readonly error: string | null;
  } | null>(null);
  const member = canonical.type === "member" ? canonical : null;
  const { sessions } = useRoom();
  const readingSession = useReadingSession();

  useEffect(() => {
    if (
      !open
      || member === null
      || (loaded?.digest === member.asset.digest && loaded.text !== null)
    ) return;
    const controller = new AbortController();
    const digest = member.asset.digest;
    setLoaded({ digest, text: null, error: null });
    const read = readingSession === null
      ? Promise.reject(new Error("No session is open to read the stored result"))
      : sessions.run(readingSession, (client) => client.readTextAsset(member.asset, controller.signal));
    void read.then(
      (text) => setLoaded({ digest, text, error: null }),
      (error: unknown) => {
        if (controller.signal.aborted) return;
        setLoaded({
          digest,
          text: null,
          error: error instanceof Error ? error.message : "stored result is unavailable",
        });
      },
    );
    return () => controller.abort();
  }, [member, open, sessions, readingSession]);

  const body = canonical.type === "inline"
    ? canonical.text
    : loaded?.digest !== canonical.asset.digest
      ? "Loading full result…"
      : loaded.error ?? loaded.text ?? "Loading full result…";
  return (
    <details className="wf-full-result" open={open} onToggle={(event) => setOpen(event.currentTarget.open)}>
      <summary className="wf-action">View full result</summary>
      {open && <pre className="tr-diag">{body}</pre>}
    </details>
  );
}

function Result({
  fact,
  onOpenPath,
}: {
  readonly fact: WorkflowFact;
  readonly onOpenPath?: ((path: PathFact) => void) | undefined;
}) {
  const run = fact.action.run ?? null;
  const canonical = run?.result ?? fact.action.callback_result ?? null;
  const text = canonical === null ? null : canonicalText(canonical);
  const allFields = canonical?.type === "inline" ? resultFields(canonical.text) : [];
  const resourcePaths = new Set(
    fact.resources.flatMap((resource) =>
      resource.resource.type === "file" ? [resource.resource.path] : [],
    ),
  );
  const fields = allFields.filter(
    (field) => !(field.label === "final doc" && resourcePaths.has(field.value)),
  );
  const resourceLabel = allFields.some(
    (field) => field.label === "final doc" && resourcePaths.has(field.value),
  ) && fact.resources.length === 1
    ? "final document"
    : "files";
  const journal = run?.journal ?? [];
  const loadedStarted = journal.filter((observation) => observation.kind === "started").length;
  const loadedReturned = journal.filter((observation) => observation.kind === "result").length;
  const journalTotal = Math.max(run?.journal_total_count ?? 0, journal.length);
  const started = Math.max(run?.journal_started_count ?? 0, loadedStarted);
  const returned = Math.max(run?.journal_result_count ?? 0, loadedReturned);
  const error = run?.error === null || run?.error === undefined ? null : canonicalText(run.error);
  if (text === null && fact.resources.length === 0 && error === null) return null;
  return (
    <section className="wf-result" aria-label="Workflow result">
      <span className="wf-section-label">Result</span>
      {fact.resources.length > 0 && (
        <div className="wf-result__resources">
          <span>{resourceLabel}</span>
          <PathList paths={fact.resources} total={fact.resources.length} onOpenPath={onOpenPath} />
        </div>
      )}
      {fields.length > 0 ? (
        <dl className="wf-output">
          {fields.map((field) => (
            <div className="wf-output__row" key={field.label}>
              <dt>{field.label}</dt>
              <dd className={field.label === "verdict" ? "wf-output__verdict" : undefined}>{field.value}</dd>
            </div>
          ))}
        </dl>
      ) : text !== null ? (
        canonical !== null && <FullResult canonical={canonical} />
      ) : null}
      {error !== null && <pre className="tr-diag wf-result__error">{error}</pre>}
      <div className="wf-result__foot">
        <span>
          terminal evidence · {run === null ? "correlated task notification" : "run snapshot + correlated task notification"}
          {journalTotal === 0 ? "" : ` · journal ${started} started, ${returned} results`}
        </span>
        {fields.length > 0 && canonical !== null && <FullResult canonical={canonical} />}
      </div>
    </section>
  );
}

export function WorkflowDetail({
  fact,
  attempts,
  children: childThreads,
  onOpenPath,
}: {
  readonly fact: WorkflowFact;
  readonly attempts: readonly WorkflowAttemptFact[];
  readonly children: readonly WorkflowChildThread[];
  readonly onOpenPath?: ((path: PathFact) => void) | undefined;
}) {
  const run = fact.action.run ?? null;
  const childMap = new Map(childThreads.map((child) => [child.childSessionId, child.node]));
  const name = run?.name ?? fact.action.name ?? "dynamic workflow";
  const groups = run === null ? [] : phaseGroups(run);
  const activePhase = groups.find((phase) => phase.workers.some((worker) => worker.state === "progress"));
  const initialPhase = activePhase?.index ?? groups.at(-1)?.index ?? null;
  const workerCount = run?.usage.agent_count ?? run?.workers.length ?? null;
  const finished = run?.updated_at ?? null;
  return (
    <div className="wf-run__detail">
      <div className="wf-run__mast">
        <div className="wf-run__title">
          <strong>{name}</strong>
          {(run?.run_id ?? fact.action.run_id) !== null && (
            <span className="wf-run__id">{run?.run_id ?? fact.action.run_id}</span>
          )}
          {finished !== null && (
            <span className="wf-run__finished">
              {run?.status === "running" ? "updated" : "finished"} {clock(finished)}
            </span>
          )}
        </div>
        {fact.objective !== null && <p className="wf-run__objective">{fact.objective}</p>}
        <div className="wf-run__facts">
          {workerCount !== null && <span><b>{workerCount}</b> workers</span>}
          {run !== null && <span><b>{run.phases.length}</b> phases</span>}
          {run?.usage.tokens !== null && run?.usage.tokens !== undefined && <span><b>{tokenText(run.usage.tokens)}</b> tokens</span>}
          {run?.usage.tool_calls !== null && run?.usage.tool_calls !== undefined && <span><b>{run.usage.tool_calls}</b> tools</span>}
          <span className="wf-run__actions">
            {fact.script !== null && (
              <button
                className="wf-action"
                type="button"
                disabled={fact.script.read === undefined || onOpenPath === undefined}
                onClick={() => {
                  if (fact.script !== null) onOpenPath?.(fact.script);
                }}
              >
                View workflow script
              </button>
            )}
            {run !== null && run.logs.length > 0 && (
              <details className="wf-logs">
                <summary className="wf-action">{quantity(run.logs.length, "log")}</summary>
                <pre className="tr-diag">{run.logs.join("\n")}</pre>
              </details>
            )}
          </span>
        </div>
      </div>
      <Attempts attempts={attempts} run={run} />
      {run !== null && groups.length > 0 && (
        <section className="wf-phases" aria-label="Workflow phases">
          <span className="wf-section-label">Phases</span>
          {groups.map((phase) => (
            <Phase
              key={`${phase.index}-${phase.title}`}
              phase={phase}
              run={run}
              current={phase.index === initialPhase}
              children={childMap}
            />
          ))}
        </section>
      )}
      <Result fact={fact} onOpenPath={onOpenPath} />
      {run === null && fact.action.arguments !== null && fact.objective === null && (
        <pre className="tr-diag">{canonicalText(fact.action.arguments)}</pre>
      )}
    </div>
  );
}
