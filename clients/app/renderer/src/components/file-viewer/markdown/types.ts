// The document shape a markdown file is parsed into. A TREE, never a string
// of HTML.
//
// That is a security boundary, not a style preference. A worktree is someone
// else's repository, and a README in it is untrusted input; `marked` plus
// `dangerouslySetInnerHTML` would let `<img src=x onerror=…>` in a checked-in
// file run inside a renderer that holds a preload bridge. Parsing to nodes
// and rendering them as React elements means there is no path from file
// content to executed markup at all — raw HTML in the source is shown as the
// text it is.

export type Alignment = "left" | "center" | "right" | null;

export type Inline =
  | { readonly type: "text"; readonly value: string; readonly source?: readonly number[] }
  | { readonly type: "code"; readonly value: string }
  | { readonly type: "strong"; readonly children: readonly Inline[] }
  | { readonly type: "emphasis"; readonly children: readonly Inline[] }
  | { readonly type: "strike"; readonly children: readonly Inline[] }
  | { readonly type: "link"; readonly href: string; readonly children: readonly Inline[] }
  | { readonly type: "image"; readonly src: string; readonly alt: string }
  | { readonly type: "break" };

export type Block =
  | { readonly type: "heading"; readonly level: number; readonly children: readonly Inline[] }
  | { readonly type: "paragraph"; readonly children: readonly Inline[] }
  | { readonly type: "code"; readonly language: string | null; readonly text: string }
  | { readonly type: "quote"; readonly children: readonly Block[] }
  | { readonly type: "rule" }
  | {
      readonly type: "list";
      readonly ordered: boolean;
      readonly start: number;
      /** A list with no blank line between its items sets tighter. */
      readonly tight: boolean;
      readonly items: readonly (readonly Block[])[];
    }
  | {
      readonly type: "table";
      readonly head: readonly (readonly Inline[])[];
      readonly align: readonly Alignment[];
      readonly rows: readonly (readonly (readonly Inline[])[])[];
    };
