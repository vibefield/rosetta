// A markdown file, rendered — or read as its own source, which is the one
// switch this view offers. Both readings are true, and a viewer that only
// offers the pretty one is hiding the file from the person who came to see
// the file.

import { useMemo } from "react";
import { parseMarkdown } from "../markdown";
import { MarkdownBlocks } from "../markdown/render";
import type { ViewBodyProps } from "../types";
import { CodeView } from "./CodeView";

export const MARKDOWN_MODES = [
  { id: "rendered", label: "Rendered" },
  { id: "source", label: "Source" },
] as const;

export function MarkdownView(props: ViewBodyProps) {
  const { file, mode } = props;
  const blocks = useMemo(
    () => (mode === "source" ? [] : parseMarkdown(file.text ?? "")),
    [file, mode],
  );

  if (mode === "source") return <CodeView {...props} />;

  return (
    <div className="ah-fileview__scroll ah-md">
      <div className="ah-md__measure ah-markdown">
        <MarkdownBlocks blocks={blocks} />
      </div>
    </div>
  );
}
