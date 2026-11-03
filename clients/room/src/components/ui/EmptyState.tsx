import type { CSSProperties, ReactNode } from "react";

export interface EmptyStateProps {
  readonly title: ReactNode;
  readonly description: ReactNode;
  readonly children?: ReactNode;
  readonly className?: string;
  readonly style?: CSSProperties;
  /** Inert hooks drawn on the root, such as the face it is
   * (`data-face`, `data-import-face`); see the DOM contract in
   * clients/app/test/dom-contract.test.mjs. */
  readonly [data: `data-${string}`]: string | undefined;
}

/** Consistent empty-state copy and spacing for room columns and lists. */
export function EmptyState({
  title,
  description,
  children,
  className,
  style,
  ...data
}: EmptyStateProps) {
  const classes = className ? `ah-empty ${className}` : "ah-empty";

  return (
    <div className={classes} style={style} {...data}>
      {children}
      <strong>{title}</strong>
      <p>{description}</p>
    </div>
  );
}
