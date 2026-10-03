import type { ReactNode } from "react";

export interface ListToolbarProps {
  readonly children: ReactNode;
}

/** Controls bar above a scrollable column list. */
export function ListToolbar({ children }: ListToolbarProps) {
  return (
    <div className="ah-listbar">
      {children}
    </div>
  );
}
