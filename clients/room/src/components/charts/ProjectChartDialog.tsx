import { useId, useRef, useState } from "react";
import type { KeyboardEvent } from "react";
import type { UsageResponse } from "@vibefield/rosetta-protocol";
import { ColumnHeading, DialogShell } from "../ui";
import { CloseGlyph, ExitFullscreenGlyph, FullscreenGlyph } from "../../lib/icons";
import { ActivityChartDetail } from "./ActivityChartDetail";
import type { ActivityChartDetailProps } from "./ActivityChartDetail";
import { ModelsChartDetail } from "./ModelsChartDetail";
import "../../styles/project-chart.css";

const VIEWS = [
  { value: "activity", label: "Activity" },
  { value: "models", label: "Models" },
] as const;
export type ProjectChartView = typeof VIEWS[number]["value"];

export interface ProjectChartDialogProps extends ActivityChartDetailProps {
  readonly initialView: ProjectChartView;
  readonly usage: UsageResponse | null;
  readonly onClose: () => void;
}

/** One modal lifetime preserves the opener, fullscreen and both detail views. */
export function ProjectChartDialog({ initialView, usage, onClose, ...activity }: ProjectChartDialogProps) {
  const [fullscreen, setFullscreen] = useState(false);
  const [view, setView] = useState(initialView);
  const [visited, setVisited] = useState(() => new Set([initialView]));
  const id = useId();
  const tabs = useRef<Partial<Record<ProjectChartView, HTMLButtonElement>>>({});
  const selectView = (next: ProjectChartView) => {
    setView(next);
    setVisited(current => current.has(next) ? current : new Set([...current, next]));
  };
  const navigateTabs = (event: KeyboardEvent<HTMLButtonElement>, current: ProjectChartView) => {
    const index = VIEWS.findIndex(option => option.value === current);
    const nextIndex = event.key === "Home" ? 0 : event.key === "End" ? VIEWS.length - 1
      : event.key === "ArrowRight" ? (index + 1) % VIEWS.length
      : event.key === "ArrowLeft" ? (index + VIEWS.length - 1) % VIEWS.length : null;
    if (nextIndex === null) return;
    event.preventDefault();
    const next = VIEWS[nextIndex]!.value;
    selectView(next);
    tabs.current[next]?.focus();
  };

  return <DialogShell title={`${activity.name} project activity and models`} onClose={onClose}
    fullscreen={fullscreen} onExitFullscreen={() => setFullscreen(false)}>
    <header className="ah-fileview__head ah-project-chart__head">
      <div className="ah-fileview__title"><ColumnHeading name={activity.name} tint={activity.color} /></div>
      <div className="ah-seg ah-project-chart__tabs" role="tablist" aria-label="Project chart details">
        {VIEWS.map(option => <button key={option.value} type="button" role="tab"
          ref={node => { if (node) tabs.current[option.value] = node; }}
          id={`${id}-tab-${option.value}`} aria-controls={`${id}-panel-${option.value}`}
          aria-selected={view === option.value} tabIndex={view === option.value ? 0 : -1}
          className={view === option.value ? "is-on" : undefined}
          onClick={() => selectView(option.value)} onKeyDown={event => navigateTabs(event, option.value)}>
          {option.label}
        </button>)}
      </div>
      <div className="ah-fileview__acts">
        <button className="ah-iconbtn" type="button" aria-pressed={fullscreen}
          title={fullscreen ? "Exit full screen (Esc)" : "View full screen"}
          aria-label={fullscreen ? "Exit full screen" : "View full screen"}
          onClick={() => setFullscreen(current => !current)}>
          {fullscreen ? <ExitFullscreenGlyph /> : <FullscreenGlyph />}
        </button>
        <button className="ah-iconbtn" type="button" title="Close (Esc)"
          aria-label="Close project charts" onClick={onClose}><CloseGlyph /></button>
      </div>
    </header>
    {/* Mount on first visit so opening Models never fetches hourly activity.
        Retain visited panels so tab switches keep selection and scroll state. */}
    {VIEWS.map(option => <div key={option.value} className="ah-project-chart__panel" role="tabpanel"
      id={`${id}-panel-${option.value}`} aria-labelledby={`${id}-tab-${option.value}`}
      hidden={view !== option.value}>
      {visited.has(option.value) && (option.value === "activity"
        ? <ActivityChartDetail {...activity} />
        : <ModelsChartDetail usage={usage} currentTimestamp={activity.currentTimestamp} />)}
    </div>)}
  </DialogShell>;
}
