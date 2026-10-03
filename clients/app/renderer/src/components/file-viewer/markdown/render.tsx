// The markdown tree, as React elements.
//
// Two rules this file exists to keep:
//
//  1. **No `dangerouslySetInnerHTML`, anywhere.** The parser never produces
//     markup and this never interprets any; a README in someone else's
//     repository cannot reach the DOM as anything but text.
//  2. **No request leaves the machine.** An `![alt](https://…)` in an
//     untrusted document is a tracking pixel with a caption, and a reader
//     opening a file should not tell a third party they did. Images render as
//     a labelled placeholder naming what they point at, which is also the
//     honest answer for the `./diagram.png` case, where the viewer has no way
//     to resolve a repository-relative path to something it may load.

import { Fragment } from "react";
import type { ReactNode } from "react";
import { highlight, languageFor, PLAIN_TEXT } from "../syntax";
import type { Block, Inline } from "./types";

/**
 * The schemes the SHELL will actually open — no wider.
 *
 * `javascript:` and `data:` can do more than navigate, and they are the two a
 * link in an untrusted document would want. `mailto:` is refused for the
 * opposite reason: the room's window-open handler passes only http(s) to the
 * OS, so a mailto link would render as a button that does nothing, and a dead
 * promise is worse than plain text. In-document anchors and
 * repository-relative paths point at nothing this viewer can reach.
 */
function safeHref(href: string): string | null {
  const trimmed = href.trim();
  return /^https?:\/\//i.test(trimmed) ? trimmed : null;
}

export type InlineTextRenderer = (value: string, source?: readonly number[]) => ReactNode;

function InlineNodes({
  nodes,
  renderText,
}: {
  readonly nodes: readonly Inline[];
  readonly renderText?: InlineTextRenderer | undefined;
}) {
  return (
    <>
      {nodes.map((node, at) => (
        <Fragment key={at}>
          {node.type === "text" ? (
            renderText === undefined ? node.value : renderText(node.value, node.source)
          ) : node.type === "code" ? (
            <code>{node.value}</code>
          ) : node.type === "strong" ? (
            <strong>
              <InlineNodes nodes={node.children} renderText={renderText} />
            </strong>
          ) : node.type === "emphasis" ? (
            <em>
              <InlineNodes nodes={node.children} renderText={renderText} />
            </em>
          ) : node.type === "strike" ? (
            <s>
              <InlineNodes nodes={node.children} renderText={renderText} />
            </s>
          ) : node.type === "break" ? (
            <br />
          ) : node.type === "image" ? (
            <span className="ah-md__image" title={node.src}>
              {node.alt === "" ? node.src : node.alt}
            </span>
          ) : (
            <LinkOrText node={node} />
          )}
        </Fragment>
      ))}
    </>
  );
}

/** A link the shell can open, or the text it was going to be. */
function LinkOrText({ node }: { readonly node: Extract<Inline, { type: "link" }> }) {
  const href = safeHref(node.href);
  if (href === null) {
    return (
      <span className="ah-md__deadlink" title={node.href}>
        <InlineNodes nodes={node.children} />
      </span>
    );
  }
  // The shell's window-open handler sends http(s) to the browser; the room
  // itself never navigates away.
  return (
    <a href={href} target="_blank" rel="noreferrer">
      <InlineNodes nodes={node.children} />
    </a>
  );
}

function CodeFence({ language, text }: { readonly language: string | null; readonly text: string }) {
  // A fence names its language the way a file name does, so the same table
  // answers both — which is the whole reason the highlighter takes a language
  // rather than a file.
  const resolved = language === null ? PLAIN_TEXT : (languageFor(`x.${language}`) ?? PLAIN_TEXT);
  const lines = highlight(text, resolved);
  return (
    <pre className="ah-md__code" data-language={language ?? undefined}>
      <code>
        {lines.map((line, row) => (
          <Fragment key={row}>
            {row > 0 && "\n"}
            {line.map((token, at) =>
              token.kind === "plain" ? (
                <Fragment key={at}>{token.text}</Fragment>
              ) : (
                <span data-t={token.kind} key={at}>
                  {token.text}
                </span>
              ),
            )}
          </Fragment>
        ))}
      </code>
    </pre>
  );
}

export function MarkdownBlocks({
  blocks,
  renderText,
}: {
  readonly blocks: readonly Block[];
  /** Optional decoration for plain text nodes. Code and link targets remain
   * literal, so callers cannot accidentally nest an action inside a link. */
  readonly renderText?: InlineTextRenderer | undefined;
}) {
  return (
    <>
      {blocks.map((block, at) => {
        switch (block.type) {
          case "heading": {
            const Tag = `h${Math.min(block.level, 6)}` as "h1";
            return (
              <Tag key={at}>
                <InlineNodes nodes={block.children} renderText={renderText} />
              </Tag>
            );
          }
          case "paragraph":
            return (
              <p key={at}>
                <InlineNodes nodes={block.children} renderText={renderText} />
              </p>
            );
          case "code":
            return <CodeFence key={at} language={block.language} text={block.text} />;
          case "quote":
            return (
              <blockquote key={at}>
                <MarkdownBlocks blocks={block.children} renderText={renderText} />
              </blockquote>
            );
          case "rule":
            return <hr key={at} />;
          case "list": {
            const items = block.items.map((item, index) => (
              <li key={index}>
                <MarkdownBlocks blocks={item} renderText={renderText} />
              </li>
            ));
            return block.ordered ? (
              <ol key={at} start={block.start} data-tight={block.tight ? "" : undefined}>
                {items}
              </ol>
            ) : (
              <ul key={at} data-tight={block.tight ? "" : undefined}>
                {items}
              </ul>
            );
          }
          case "table":
            return (
              // A wide table scrolls inside its own box; the document's
              // measure is not the table's to widen.
              <div className="ah-md__tablebox" key={at}>
                <table>
                  <thead>
                    <tr>
                      {block.head.map((cell, column) => (
                        <th key={column} style={{ textAlign: block.align[column] ?? undefined }}>
                          <InlineNodes nodes={cell} renderText={renderText} />
                        </th>
                      ))}
                    </tr>
                  </thead>
                  <tbody>
                    {block.rows.map((row, index) => (
                      <tr key={index}>
                        {row.map((cell, column) => (
                          <td key={column} style={{ textAlign: block.align[column] ?? undefined }}>
                            <InlineNodes nodes={cell} renderText={renderText} />
                          </td>
                        ))}
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            );
        }
      })}
    </>
  );
}
