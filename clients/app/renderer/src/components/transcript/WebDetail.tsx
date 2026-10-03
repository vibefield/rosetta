import { useState } from "react";
import type { FetchFact, SourceFact } from "../../lib/transcript";
import { useSetSiteIconsEnabled, useSiteIconsEnabled } from "../../lib/transcript/favicons";
import { webAddress } from "../../lib/transcript/web-address";
import { Favicon } from "./Favicon";

/** One shared preference, exposed beside the content it controls. */
function SiteIconsSetting() {
  const enabled = useSiteIconsEnabled();
  const setSiteIconsEnabled = useSetSiteIconsEnabled();
  return (
    <label
      className="tr-site-icons"
      title="Load icons directly from their sites. Applies to all web activity."
    >
      <input
        type="checkbox"
        checked={enabled}
        onChange={(event) => setSiteIconsEnabled(event.target.checked)}
      />
      Fetch site icons
    </label>
  );
}

const SOURCE_PREVIEW_LIMIT = 24;

export function SourceList({ sources, total }: {
  readonly sources: readonly SourceFact[];
  readonly total: number | null;
}) {
  const [expanded, setExpanded] = useState(false);
  const shown = expanded ? sources : sources.slice(0, SOURCE_PREVIEW_LIMIT);
  const omitted = total === null ? null : Math.max(0, total - sources.length);
  return (
    <>
      {shown.length > 0 && (
        <ul className="tr-src">
          {shown.map((source, at) => {
            const address = webAddress(source.url);
            const title = source.title || source.url;
            const content = (
              <>
                <Favicon host={address?.host ?? null} />
                <span className="title">{title}</span>
                <span className="dom">{address?.host ?? "unknown site"}</span>
              </>
            );
            return (
              <li key={`${source.url}-${at}`}>
                {address ? (
                  <a
                    className="tr-src__row"
                    href={address.href}
                    target="_blank"
                    rel="noreferrer"
                    title={`${title}\n${source.url}`}
                  >
                    {content}
                  </a>
                ) : (
                  <span className="tr-src__row" title={source.url}>{content}</span>
                )}
              </li>
            );
          })}
        </ul>
      )}
      {sources.length === 0 && (
        <p className="tr-web-note">
          {total === 0 ? "No sources found." : "No source details recorded."}
        </p>
      )}
      {sources.length > SOURCE_PREVIEW_LIMIT && (
        <button
          className="tr-web-more"
          type="button"
          aria-expanded={expanded}
          onClick={() => setExpanded(!expanded)}
        >
          {expanded ? "Show fewer sources" : `Show all ${sources.length} recorded sources`}
        </button>
      )}
      {omitted !== null && omitted > 0 && (
        <p className="tr-web-note">
          {omitted} additional {omitted === 1 ? "source was" : "sources were"} not retained.
        </p>
      )}
      {total === null && sources.length > 0 && (
        <p className="tr-web-note">Some source details were not recorded.</p>
      )}
      <SiteIconsSetting />
    </>
  );
}

export function FetchDetails({ fetches }: { readonly fetches: readonly FetchFact[] }) {
  return (
    <>
      {fetches.map((fetch, at) => {
        const address = webAddress(fetch.url);
        return (
          <dl className="tr-kv tr-fetch" key={at}>
            {fetch.url !== null && (
              <>
                <dt>url</dt>
                <dd>
                  {address ? (
                    <a href={address.href} target="_blank" rel="noreferrer">{fetch.url}</a>
                  ) : fetch.url}
                </dd>
              </>
            )}
            {fetch.title !== null && (
              <>
                <dt>title</dt>
                <dd data-prose=""><Favicon host={address?.host ?? null} /> {fetch.title}</dd>
              </>
            )}
            {fetch.status !== null && <><dt>status</dt><dd>HTTP {fetch.status}</dd></>}
            {fetch.url === null && fetch.title === null && fetch.status === null && (
              <><dt>page</dt><dd>No page details recorded.</dd></>
            )}
          </dl>
        );
      })}
      <SiteIconsSetting />
    </>
  );
}
