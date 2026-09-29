import { createContext } from "react";

/** Deep workers open at the column's full measure instead of adding rails. */
export const ThreadNavigationContext = createContext<{
  readonly depth: number;
  readonly open: ((sessionId: string) => void) | null;
}>({ depth: 0, open: null });
