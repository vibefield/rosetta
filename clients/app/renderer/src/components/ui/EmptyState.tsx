import type { CSSProperties, ReactNode } from "react";

export interface EmptyStateProps {
  readonly title: ReactNode;
  readonly description: ReactNode;
  readonly children?: ReactNode;
  readonly className?: string;
  readonly style?: CSSProperties;
}

/** Consistent empty-state copy and spacing for room columns and lists. */
export function EmptyState({
  title,
  description,
  children,
  className,
  style,
}: EmptyStateProps) {
  const classes = className ? `ah-empty ${className}` : "ah-empty";

  return (
    <div className={classes} style={style}>
      {children}
      <strong>{title}</strong>
      <p>{description}</p>
    </div>
  );
}
