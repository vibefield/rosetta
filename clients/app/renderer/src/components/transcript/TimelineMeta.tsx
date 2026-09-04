// §05A — timeline_meta is the map back into the transcript, never a second
// rendering of it. Three floating launchers share one non-modal island;
// tasks land on their retained message and file rows hand off to the room's
// existing viewer.

import { useEffect, useId, useRef, useState } from "react";
import type {
  TimelineMeta as TimelineMetaValue,
  TimelineMetaArtifact,
  TimelineMetaPlan,
  TimelineMetaTask,
} from "@rosetta/protocol";
import { clockShort, fmtBytes } from "../../lib/format";
import { FamilyGlyph, LineChevron } from "./glyphs";

type MetaKind = "tasks" | "plans" | "artifacts";

const KINDS: readonly MetaKind[] = ["tasks", "plans", "artifacts"];
const GLYPH = { tasks: "task", plans: "doc", artifacts: "box" } as const;

function exactCount(meta: TimelineMetaValue, kind: MetaKind): number {
  return meta[kind].length;
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

function taskDuration(task: TimelineMetaTask, now: number): string {
  if (task.started_at === null) return "—";
  if (task.status === "in_progress") {
    return now < task.started_at ? "—" : `${spanText(now - task.started_at)} now`;
  }
  if (task.terminal_at === null) return "—";
  if (task.terminal_at < task.started_at) return "—";
  return spanText(task.terminal_at - task.started_at);
}

function fileType(name: string): string {
  const extension = name.split(".").pop();
  return extension === undefined || extension === name ? "File" : extension.toUpperCase();
}

function EmptyPanel({ kind }: { readonly kind: MetaKind }) {
  return <p className="tr-meta-empty">No {kind} recorded in this session.</p>;
}

function TaskPanel({
  tasks,
  now,
  onOpen,
}: {
  readonly tasks: readonly TimelineMetaTask[];
  readonly now: number;
  readonly onOpen: (task: TimelineMetaTask) => void;
}) {
  if (tasks.length === 0) return <EmptyPanel kind="tasks" />;
  return (
    <>
      <div className="tr-meta-listhead">
        <span>Task · state · started</span>
        <span>Duration</span>
      </div>
      <ol className="tr-meta-list" aria-label="Full task list">
        {tasks.map((task) => {
          const status = taskStatus(task);
          const started = task.started_at === null ? "start not recorded" : `started ${clockShort(task.started_at)}`;
          const duration = taskDuration(task, now);
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
  readonly onOpenPlan: (plan: TimelineMetaPlan) => void;
  readonly onOpenArtifact: (artifact: TimelineMetaArtifact) => void;
}

export function TimelineMeta({ meta, onOpenTask, onOpenPlan, onOpenArtifact }: TimelineMetaProps) {
  const [active, setActive] = useState<MetaKind | null>(null);
  const [now, setNow] = useState(Date.now());
  const rootRef = useRef<HTMLDivElement | null>(null);
  const launcherRefs = useRef<Partial<Record<MetaKind, HTMLButtonElement | null>>>({});
  const id = useId().replaceAll(":", "");

  useEffect(() => {
    if (active !== "tasks" || !meta.tasks.some((task) => task.status === "in_progress")) return;
    setNow(Date.now());
    const timer = window.setInterval(() => setNow(Date.now()), 1_000);
    return () => window.clearInterval(timer);
  }, [active, meta.tasks]);

  useEffect(() => {
    if (active === null) return;
    const pointerDown = (event: PointerEvent) => {
      if (!rootRef.current?.contains(event.target as Node)) setActive(null);
    };
    const keyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      const selected = active;
      setActive(null);
      launcherRefs.current[selected]?.focus();
    };
    document.addEventListener("pointerdown", pointerDown);
    document.addEventListener("keydown", keyDown);
    return () => {
      document.removeEventListener("pointerdown", pointerDown);
      document.removeEventListener("keydown", keyDown);
    };
  }, [active]);

  const selectTab = (kind: MetaKind) => setActive(kind);
  const moveTab = (kind: MetaKind, direction: -1 | 1) => {
    const index = KINDS.indexOf(kind);
    const next = KINDS[(index + direction + KINDS.length) % KINDS.length];
    setActive(next);
    document.getElementById(`${id}-tab-${next}`)?.focus();
  };

  return (
    <div className="tr-meta" ref={rootRef}>
      <div className="tr-meta-launchers" role="group" aria-label="Session work">
        {KINDS.map((kind) => {
          const count = exactCount(meta, kind);
          return (
            <button
              key={kind}
              ref={(element) => { launcherRefs.current[kind] = element; }}
              className="tr-meta-launch"
              type="button"
              aria-label={`${titleCase(kind)}, ${count}`}
              aria-expanded={active === kind}
              aria-controls={`${id}-popover`}
              onClick={() => setActive((current) => current === kind ? null : kind)}
            >
              <FamilyGlyph glyph={GLYPH[kind]} />
              <span className="tr-meta-launch__count">{count}</span>
            </button>
          );
        })}
      </div>

      {active !== null && (
        <div
          className="tr-meta-popover"
          id={`${id}-popover`}
          role="dialog"
          aria-modal="false"
          aria-label="Session work index"
        >
          <div className="tr-meta-top">
            <div className="tr-meta-tabs" role="tablist" aria-label="Session work type">
              {KINDS.map((kind) => (
                <button
                  className="tr-meta-tab"
                  id={`${id}-tab-${kind}`}
                  key={kind}
                  type="button"
                  role="tab"
                  aria-selected={active === kind}
                  aria-controls={`${id}-panel`}
                  tabIndex={active === kind ? 0 : -1}
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
                const selected = active;
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
            aria-labelledby={`${id}-tab-${active}`}
          >
            {active === "tasks" && (
              <TaskPanel
                tasks={meta.tasks}
                now={now}
                onOpen={(task) => {
                  setActive(null);
                  onOpenTask(task);
                }}
              />
            )}
            {active === "plans" && (
              <PlanPanel
                plans={meta.plans}
                onOpen={(plan) => {
                  setActive(null);
                  onOpenPlan(plan);
                }}
              />
            )}
            {active === "artifacts" && (
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
