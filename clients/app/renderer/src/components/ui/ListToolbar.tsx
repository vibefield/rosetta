import type { ReactNode } from "react";

export interface ListToolbarProps {
  readonly label: ReactNode;
  readonly children?: ReactNode;
}

/** Shared label-and-controls bar above a scrollable column list. */
export function ListToolbar({ label, children }: ListToolbarProps) {
  return (
    <div className="ah-listbar">
      <p className="ah-colnote">{label}</p>
      {children}
    </div>
  );
}
