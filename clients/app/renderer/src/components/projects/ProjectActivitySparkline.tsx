import type { CSSProperties } from "react";

type ActivityBarStyle = CSSProperties & { "--h": string };

export interface ProjectActivitySparklineProps {
  readonly values: readonly number[];
  readonly accessibleLabel?: string;
}

/** Compact bar sparkline for a project's recent token buckets. */
export function ProjectActivitySparkline({
  values,
  accessibleLabel = "Token activity over the last 30 days",
}: ProjectActivitySparklineProps) {
  const peak = values.reduce(
    (maximum, value) => Math.max(maximum, Number.isFinite(value) ? value : 0),
    0,
  );

  return (
    <div
      className="ah-project-activity-sparkline"
      role="img"
      aria-label={accessibleLabel}
    >
      {values.map((rawValue, bucketIndex) => {
        const value = Number.isFinite(rawValue) ? Math.max(0, rawValue) : 0;
        const height = value === 0 || peak === 0
          ? "1px"
          : `${Math.max(12, Math.round((value / peak) * 100))}%`;

        return (
          <i
            key={bucketIndex}
            data-zero={value === 0 ? "" : undefined}
            style={{ "--h": height } as ActivityBarStyle}
          />
        );
      })}
    </div>
  );
}
