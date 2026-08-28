import type { ReactNode } from "react";

export interface EmptyStateProps {
  readonly title: ReactNode;
  readonly description: ReactNode;
  readonly children?: ReactNode;
}

/** Consistent empty-state copy and spacing for room columns and lists. */
export function EmptyState({ title, description, children }: EmptyStateProps) {
  return (
    <div className="ah-empty">
      {children}
      <strong>{title}</strong>
      <p>{description}</p>
    </div>
  );
}
