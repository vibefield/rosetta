// §05A — timeline_meta is the map back into the transcript, never a second
// rendering of it. Up to four non-empty launchers share one non-modal island;
// tasks and Workflow runs land on retained messages, while file rows hand off
// to the room's existing viewer.

import { useEffect, useId, useRef, useState } from "react";
import type {
  TimelineMeta as TimelineMetaValue,
  TimelineMetaArtifact,
  TimelineMetaPlan,
  TimelineMetaTask,
  TimelineMetaWorkflow,
} from "@vibefield/rosetta-protocol";
import { clockShort, fmtBytes } from "../../lib/format";
import { FamilyGlyph, LineChevron } from "./glyphs";
import {
  availableTimelineMetaKinds,
  resolveTimelineMetaKind,
  recordedTaskDuration,
  recordedWorkflowDuration,
  type TimelineMetaKind,
} from "./timeline-meta-state";

const GLYPH = { tasks: "task", workflows: "bolt", plans: "doc", artifacts: "box" } as const;

function exactCount(meta: TimelineMetaValue, kind: TimelineMetaKind): number {
  return (meta[kind] ?? []).length;
}

function titleCase(value: string): string {
  return value.replaceAll("_", " ").replace(/^./, (letter) => letter.toUpperCase());
}

function taskState(task: TimelineMetaTask): string {
  if (task.status === "in_progress") return "running";
  if (task.status === "failed" || task.status === "cancelled") return "failed";
  return task.status ?? "unknown";
}

function taskStatus(task: TimelineMetaTask): string {
  return task.status === null ? "State not recorded" : titleCase(task.status);
}

function spanText(milliseconds: number): string {
  const seconds = Math.max(0, Math.floor(milliseconds / 1_000));
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ${String(seconds % 60).padStart(2, "0")}s`;
  const hours = Math.floor(minutes / 60);
  if (hours < 48) return `${hours}h ${String(minutes % 60).padStart(2, "0")}m`;
  return `${Math.floor(hours / 24)}d ${hours % 24}h`;
}

function workflowState(workflow: TimelineMetaWorkflow): string {
  if (workflow.status === "running") return "running";
  if (workflow.status === "failed") return "failed";
  return workflow.status;
}

function fileType(name: string): string {
  const extension = name.split(".").pop();
  return extension === undefined || extension === name ? "File" : extension.toUpperCase();
}

function EmptyPanel({ kind }: { readonly kind: TimelineMetaKind }) {
  return <p className="tr-meta-empty">No {kind} recorded in this session.</p>;
}

function TaskPanel({
  tasks,
  onOpen,
}: {
  readonly tasks: readonly TimelineMetaTask[];
  readonly onOpen: (task: TimelineMetaTask) => void;
}) {
  if (tasks.length === 0) return <EmptyPanel kind="tasks" />;
  return (
    <>
      <div className="tr-meta-listhead">
        <span>Task · last recorded state · started</span>
        <span>Duration</span>
      </div>
      <ol className="tr-meta-list" aria-label="Full task list">
        {tasks.map((task) => {
          const status = taskStatus(task);
          const started = task.started_at === null ? "start not recorded" : `started ${clockShort(task.started_at)}`;
          const durationMs = recordedTaskDuration(task);
          const duration = durationMs === null ? "—" : spanText(durationMs);
          const openable = task.timeline_id !== null;
          return (
            <li key={task.id}>
              <button
                className="tr-meta-row"
                type="button"
                data-state={taskState(task)}
                disabled={!openable}
                aria-label={`${task.subject}, ${status}, ${started}, duration ${duration}${openable ? "; jump to task message" : "; no task message anchor recorded"}`}
                onClick={() => onOpen(task)}
              >
                <span className="tr-meta-status" aria-hidden="true">
                  {task.status === "completed" ? "✓" : ""}
                </span>
                <span className="tr-meta-row__body">
                  <span className="tr-meta-row__name">{task.subject}</span>
                  <span className="tr-meta-row__meta">{status} · {started}</span>
                </span>
                <span className="tr-meta-row__value">{duration}</span>
                <span className="tr-meta-row__chev"><LineChevron /></span>
              </button>
            </li>
          );
        })}
      </ol>
    </>
  );
}

function WorkflowPanel({
  workflows,
  onOpen,
}: {
  readonly workflows: readonly TimelineMetaWorkflow[];
  readonly onOpen: (workflow: TimelineMetaWorkflow) => void;
}) {
  if (workflows.length === 0) return <EmptyPanel kind="workflows" />;
  return (
    <>
      <div className="tr-meta-listhead">
        <span>Workflow · last recorded state · started</span>
        <span>Duration</span>
      </div>
      <ol className="tr-meta-list" aria-label="Workflow runs">
        {workflows.map((workflow) => {
          const status = titleCase(workflow.status);
          const started = workflow.started_at === null
            ? "start not recorded"
            : `started ${clockShort(workflow.started_at)}`;
          const durationMs = recordedWorkflowDuration(workflow);
          const duration = durationMs === null ? "—" : spanText(durationMs);
          const workers = workflow.worker_count === null
            ? "worker count not recorded"
            : `${workflow.worker_count} ${workflow.worker_count === 1 ? "worker" : "workers"}`;
          const openable = workflow.timeline_id !== null;
          return (
            <li key={workflow.id}>
              <button
                className="tr-meta-row"
                type="button"
                data-kind="workflow"
                data-state={workflowState(workflow)}
                disabled={!openable}
                aria-label={`${workflow.name}, ${status}, ${started}, ${workers}, duration ${duration}${openable ? "; jump to workflow message" : "; no workflow message anchor recorded"}`}
                onClick={() => onOpen(workflow)}
              >
                <span className="tr-meta-status" aria-hidden="true">
                  {workflow.status === "completed" ? "✓" : ""}
                </span>
                <span className="tr-meta-row__body">
                  <span className="tr-meta-row__name">{workflow.name}</span>
                  <span className="tr-meta-row__meta">{status} · {started} · {workers}</span>
                </span>
                <span className="tr-meta-row__value">{duration}</span>
                <span className="tr-meta-row__chev"><LineChevron /></span>
              </button>
            </li>
          );
        })}
      </ol>
    </>
  );
}

function PlanPanel({
  plans,
  onOpen,
}: {
  readonly plans: readonly TimelineMetaPlan[];
  readonly onOpen: (plan: TimelineMetaPlan) => void;
}) {
  if (plans.length === 0) return <EmptyPanel kind="plans" />;
  return (
    <>
      <div className="tr-meta-listhead">
        <span>Plan file · updated</span>
        <span>Size</span>
      </div>
      <ol className="tr-meta-list" aria-label="Plan files">
        {plans.map((plan) => (
          <li key={plan.id}>
            <button
              className="tr-meta-row"
              type="button"
              aria-label={`Open ${plan.name} in file viewer`}
              onClick={() => onOpen(plan)}
            >
              <span className="tr-meta-file"><FamilyGlyph glyph="doc" /></span>
              <span className="tr-meta-row__body">
                <span className="tr-meta-row__name">{plan.name}</span>
                <span className="tr-meta-row__meta">
                  {fileType(plan.name)} · {plan.updated_at === null ? "update time not recorded" : `updated ${clockShort(plan.updated_at)}`}
                </span>
              </span>
              <span className="tr-meta-row__value">{fmtBytes(plan.byte_len)}</span>
              <span className="tr-meta-row__chev"><LineChevron /></span>
            </button>
          </li>
        ))}
      </ol>
    </>
  );
}

function ArtifactPanel({
  artifacts,
  onOpen,
}: {
  readonly artifacts: readonly TimelineMetaArtifact[];
  readonly onOpen: (artifact: TimelineMetaArtifact) => void;
}) {
  if (artifacts.length === 0) return <EmptyPanel kind="artifacts" />;
  return (
    <>
      <div className="tr-meta-listhead">
        <span>Artifact · created</span>
        <span>Size</span>
      </div>
      <ol className="tr-meta-list" aria-label="Artifact files">
        {artifacts.map((artifact) => (
          <li key={artifact.id}>
            <button
              className="tr-meta-row"
              type="button"
              aria-label={`Open ${artifact.name} in file viewer`}
              onClick={() => onOpen(artifact)}
            >
              <span className="tr-meta-file"><FamilyGlyph glyph="doc" /></span>
              <span className="tr-meta-row__body">
                <span className="tr-meta-row__name">{artifact.name}</span>
                <span className="tr-meta-row__meta">
                  {titleCase(artifact.artifact_kind)} · {artifact.created_at === null ? "creation time not recorded" : `created ${clockShort(artifact.created_at)}`}
                </span>
              </span>
              <span className="tr-meta-row__value">
                {artifact.byte_len === null ? "—" : fmtBytes(artifact.byte_len)}
              </span>
              <span className="tr-meta-row__chev"><LineChevron /></span>
            </button>
          </li>
        ))}
      </ol>
    </>
  );
}

export interface TimelineMetaProps {
  readonly meta: TimelineMetaValue;
  readonly onOpenTask: (task: TimelineMetaTask) => void;
  readonly onOpenWorkflow: (workflow: TimelineMetaWorkflow) => void;
  readonly onOpenPlan: (plan: TimelineMetaPlan) => void;
  readonly onOpenArtifact: (artifact: TimelineMetaArtifact) => void;
}

export function TimelineMeta({
  meta,
  onOpenTask,
  onOpenWorkflow,
  onOpenPlan,
  onOpenArtifact,
}: TimelineMetaProps) {
  const [active, setActive] = useState<TimelineMetaKind | null>(null);
  const rootRef = useRef<HTMLDivElement | null>(null);
  const launcherRefs = useRef<Partial<Record<TimelineMetaKind, HTMLButtonElement | null>>>({});
  const id = useId().replaceAll(":", "");
  const availableKinds = availableTimelineMetaKinds(meta);
  const visibleActive = resolveTimelineMetaKind(active, availableKinds);

  useEffect(() => {
    if (active !== visibleActive) setActive(visibleActive);
  }, [active, visibleActive]);

  useEffect(() => {
    if (visibleActive === null) return;
    const pointerDown = (event: PointerEvent) => {
      if (!rootRef.current?.contains(event.target as Node)) setActive(null);
    };
    const keyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      const selected = visibleActive;
      setActive(null);
      launcherRefs.current[selected]?.focus();
    };
    document.addEventListener("pointerdown", pointerDown);
    document.addEventListener("keydown", keyDown);
    return () => {
      document.removeEventListener("pointerdown", pointerDown);
      document.removeEventListener("keydown", keyDown);
    };
  }, [visibleActive]);

  const selectTab = (kind: TimelineMetaKind) => setActive(kind);
  const moveTab = (kind: TimelineMetaKind, direction: -1 | 1) => {
    const index = availableKinds.indexOf(kind);
    const next = availableKinds[(index + direction + availableKinds.length) % availableKinds.length];
    if (next === undefined) return;
    setActive(next);
    document.getElementById(`${id}-tab-${next}`)?.focus();
  };

  if (availableKinds.length === 0) return null;

  return (
    <div className="tr-meta" ref={rootRef}>
      <div className="tr-meta-launchers" role="group" aria-label="Session work">
        {availableKinds.map((kind) => {
          const count = exactCount(meta, kind);
          return (
            <button
              key={kind}
              ref={(element) => { launcherRefs.current[kind] = element; }}
              className="tr-meta-launch"
              type="button"
              aria-label={`${titleCase(kind)}, ${count}`}
              aria-expanded={visibleActive === kind}
              aria-controls={`${id}-popover`}
              onClick={() => setActive((current) => current === kind ? null : kind)}
            >
              <FamilyGlyph glyph={GLYPH[kind]} />
              <span className="tr-meta-launch__count">{count}</span>
            </button>
          );
        })}
      </div>

      {visibleActive !== null && (
        <div
          className="tr-meta-popover"
          id={`${id}-popover`}
          role="dialog"
          aria-modal="false"
          aria-label="Session work index"
        >
          <div className="tr-meta-top">
            <div className="tr-meta-tabs" role="tablist" aria-label="Session work type">
              {availableKinds.map((kind) => (
                <button
                  className="tr-meta-tab"
                  id={`${id}-tab-${kind}`}
                  key={kind}
                  type="button"
                  role="tab"
                  aria-selected={visibleActive === kind}
                  aria-controls={`${id}-panel`}
                  tabIndex={visibleActive === kind ? 0 : -1}
                  onClick={() => selectTab(kind)}
                  onKeyDown={(event) => {
                    if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
                    event.preventDefault();
                    moveTab(kind, event.key === "ArrowRight" ? 1 : -1);
                  }}
                >
                  {titleCase(kind)}
                </button>
              ))}
            </div>
            <button
              className="tr-meta-close"
              type="button"
              aria-label="Close session work"
              onClick={() => {
                const selected = visibleActive;
                setActive(null);
                launcherRefs.current[selected]?.focus();
              }}
            >
              <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" aria-hidden="true">
                <path d="m4.5 4.5 7 7M11.5 4.5l-7 7" />
              </svg>
            </button>
          </div>

          <div
            className="tr-meta-panel"
            id={`${id}-panel`}
            role="tabpanel"
            aria-labelledby={`${id}-tab-${visibleActive}`}
          >
            {visibleActive === "tasks" && (
              <TaskPanel
                tasks={meta.tasks}
                onOpen={(task) => {
                  setActive(null);
                  onOpenTask(task);
                }}
              />
            )}
            {visibleActive === "workflows" && (
              <WorkflowPanel
                workflows={meta.workflows}
                onOpen={(workflow) => {
                  setActive(null);
                  onOpenWorkflow(workflow);
                }}
              />
            )}
            {visibleActive === "plans" && (
              <PlanPanel
                plans={meta.plans}
                onOpen={(plan) => {
                  setActive(null);
                  onOpenPlan(plan);
                }}
              />
            )}
            {visibleActive === "artifacts" && (
              <ArtifactPanel
                artifacts={meta.artifacts}
                onOpen={(artifact) => {
                  setActive(null);
                  onOpenArtifact(artifact);
                }}
              />
            )}
          </div>
        </div>
      )}
    </div>
  );
}
