import * as Dialog from "@radix-ui/react-dialog";
import { useEffect, useMemo, useRef, useState } from "react";
import type { ArtifactEntry, ArtifactListRequest, ArtifactPage, HtmlArtifactAvailability } from "@vibefield/rosetta-protocol";
import { ApiRequestError } from "@vibefield/rosetta-protocol";
import type { CatalogPort } from "../../ports";
import { useRoom, useRoomOverlay, useRoomPortal } from "../../room-context";
import { AgentMark, agentLabel } from "../../lib/agents";
import { ARTIFACT_SANDBOX, artifactDocument, publishedArtifactUrl } from "./preview";
import { ArtifactContentCache } from "./content";
import { loadAllArtifacts } from "./load-all";
import { artifactThumbnails } from "./thumbnails";

type LibraryPort = NonNullable<CatalogPort["artifacts"]>;
const AVAILABILITY: Record<HtmlArtifactAvailability, string> = {
  captured: "Captured HTML",
  not_captured: "HTML was not captured",
  missing: "Source file was missing at capture",
  too_large: "Source exceeds the 16 MiB limit",
  unsupported: "Source could not be captured as HTML",
  changed_during_read: "Source changed during capture",
};
const when = (at: number | null) => at === null ? "Date unavailable" : new Date(at).toLocaleString();
const errorText = (error: unknown) => error instanceof Error ? error.message : "Could not load artifacts";

export function ArtifactGlyph() {
  return <svg viewBox="0 0 18 18" fill="none" stroke="currentColor" strokeWidth="1.35" aria-hidden="true"><rect x="3.5" y="2.5" width="12" height="11" rx="2" /><path d="M12 15.5H4a2 2 0 0 1-2-2V6M7 6.5 5.5 8 7 9.5m5-3L13.5 8 12 9.5" /></svg>;
}

function useArtifacts(port: LibraryPort, request: ArtifactListRequest) {
  const [page, setPage] = useState<ArtifactPage | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [refresh, setRefresh] = useState(0);
  const key = JSON.stringify(request);
  useEffect(() => {
    const controller = new AbortController();
    setLoading(true); setError(null); setPage(null);
    const timer = setTimeout(() => {
      void loadAllArtifacts(port, request, controller.signal, setPage).catch((error: unknown) => {
        if (controller.signal.aborted) return;
        setError(error instanceof ApiRequestError && error.code === "cursor_expired"
          ? "The library is still changing. Refresh to load the complete list." : errorText(error));
      }).finally(() => { if (!controller.signal.aborted) setLoading(false); });
    }, request.q ? 180 : 0);
    return () => { clearTimeout(timer); controller.abort(); };
  }, [port, key, refresh]);
  return { page, error, loading, refresh: () => setRefresh((n) => n + 1) };
}

function ArtifactPreview({ entry, port, cache, onSession, onBack }: { readonly entry: ArtifactEntry; readonly port: LibraryPort; readonly cache: ArtifactContentCache; readonly onSession: () => void; readonly onBack: () => void }) {
  const { host } = useRoom();
  const [view, setView] = useState<"preview" | "source">("preview");
  const [bytes, setBytes] = useState<Uint8Array | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [attempt, setAttempt] = useState(0);
  const [opening, setOpening] = useState(false);
  const publication = entry.publication;
  const publishedUrl = publishedArtifactUrl(publication.url);
  useEffect(() => {
    const controller = new AbortController();
    setBytes(null); setError(null);
    if (entry.availability === "captured") {
      void cache.read(port, entry, controller.signal).then((result) => {
        if (controller.signal.aborted) return;
        setBytes(result);
      }).catch((error: unknown) => { if (!controller.signal.aborted) setError(errorText(error)); });
    }
    return () => controller.abort();
  }, [port, cache, entry, attempt]);
  const text = useMemo(() => bytes === null ? null : new TextDecoder().decode(bytes), [bytes]);
  const document = useMemo(() => text === null ? undefined : artifactDocument(text), [text]);
  const openBrowser = async () => {
    if (!host || !bytes) return;
    setOpening(true); setError(null);
    try { if (!await host.openInBrowser(publication.filename, bytes)) setError("Could not open this artifact in your browser"); }
    catch (error) { setError(errorText(error)); }
    finally { setOpening(false); }
  };
  return <section className="ah-artifacts__detail" aria-label={publication.title}>
    <div className="ah-artifacts__detailhead">
      <button type="button" onClick={onBack}>← Back to grid</button>
      <div><h3>{publication.title}</h3><span className="ah-artifacts__filename">{publication.filename}</span></div>
      <button type="button" onClick={onSession}>Open session ↗</button>
    </div>
    <div className="ah-artifacts__provenance">
      <span><AgentMark kind={entry.agent} /> {agentLabel(entry.agent)} · {entry.project_name ?? "No project"}</span>
      <span title={entry.session_title ?? publication.session_id}>{entry.session_title ?? "Agent session"}</span>
      <span>Presented {when(publication.published_at)}{publication.version === null ? "" : ` · Version ${publication.version}`}</span>
      {entry.captured_at !== null && <span>File captured {when(entry.captured_at)}</span>}
      {entry.recovered_from && <span title={entry.recovered_from}>Recovered copy · {entry.recovered_from}</span>}
    </div>
    <div className="ah-artifacts__toolbar">
      <div role="group" aria-label="Artifact view" hidden={entry.availability !== "captured"}>
        <button type="button" aria-pressed={view === "preview"} onClick={() => setView("preview")}>Preview</button>
        <button type="button" aria-pressed={view === "source"} onClick={() => setView("source")}>HTML source</button>
      </div>
      {host && bytes && <button type="button" disabled={opening} onClick={() => { void openBrowser(); }}>Open in browser ↗</button>}
      {publishedUrl && <a href={publishedUrl} target="_blank" rel="noopener noreferrer">Open published artifact ↗</a>}
    </div>
    {error !== null && <div className="ah-artifacts__message" role="alert">{error} <button type="button" onClick={() => setAttempt((n) => n + 1)}>Retry</button></div>}
    {entry.availability !== "captured" ? <div className="ah-artifacts__empty"><ArtifactGlyph /><h3>{publishedUrl ? "Available through its published link" : AVAILABILITY[entry.availability]}</h3><p>{publishedUrl ? "A local preview is unavailable. Open the published artifact in your browser." : "No local copy or published link is available. Open its session to review the original context."}</p>{publishedUrl && <a className="ah-artifacts__published" href={publishedUrl} target="_blank" rel="noopener noreferrer">Open published artifact ↗</a>}</div>
      : bytes === null ? <div className="ah-artifacts__empty" role="status">{error === null ? "Loading captured HTML…" : "Preview unavailable"}</div>
      : view === "source" ? <pre className="ah-artifacts__source">{text!.slice(0, 1_000_000)}{text!.length > 1_000_000 ? "\n\n[Source display limited to 1 million characters]" : ""}</pre>
      : <iframe className="ah-artifacts__preview" title={`Preview: ${publication.title}`} sandbox={ARTIFACT_SANDBOX} referrerPolicy="no-referrer" srcDoc={document} />}
    <p className="ah-artifacts__note">{entry.recovered_from ? "Recovered copy. It may differ from the published version." : entry.availability === "captured" ? "Captured file snapshot. External data and local file dependencies may be unavailable." : publishedUrl ? "Published pages may change or require you to sign in." : "The artifact’s presentation remains recorded in its session."}</p>
  </section>;
}

function ArtifactThumbnail({ entry, port, active }: { readonly entry: ArtifactEntry; readonly port: LibraryPort; readonly active: boolean }) {
  const { host } = useRoom();
  const container = useRef<HTMLSpanElement>(null);
  const [visible, setVisible] = useState(false);
  const [image, setImage] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    const element = container.current;
    if (!element) return;
    const observer = new IntersectionObserver(([entry]) => setVisible(entry?.isIntersecting ?? false));
    observer.observe(element);
    return () => observer.disconnect();
  }, []);
  useEffect(() => {
    if (!active || !visible || entry.availability !== "captured") {
      setImage(null);
      return;
    }
    if (!host?.artifactThumbnail) { setFailed(true); return; }
    const controller = new AbortController();
    setFailed(false);
    void artifactThumbnails(port).read(port, entry, host.artifactThumbnail, controller.signal).then((image) => {
      if (!controller.signal.aborted) { setImage(image); setFailed(image === null); }
    }).catch(() => { if (!controller.signal.aborted) setFailed(true); });
    return () => controller.abort();
  }, [active, visible, entry, port, host]);
  const online = publishedArtifactUrl(entry.publication.url) !== null;
  return <span ref={container} className="ah-artifacts__thumbnail" aria-hidden="true">
    {active && visible && image && !failed ? <img alt="" src={image} decoding="async" onError={() => setFailed(true)} />
      : <span className="ah-artifacts__placeholder"><ArtifactGlyph /><span>{entry.availability === "captured" ? failed ? "Preview unavailable" : "Loading preview…" : online ? "Published online" : "No local preview"}</span></span>}
  </span>;
}

export function ArtifactLibrary({ open, onClose, returnFocus, onOpenSession, agents, projects }: {
  readonly open: boolean;
  readonly onClose: () => void;
  readonly returnFocus: () => void;
  readonly onOpenSession: (sessionId: string, timelineId: string) => void;
  readonly agents: readonly string[];
  readonly projects: readonly { id: string; name: string }[];
}) {
  const { catalog } = useRoom();
  const container = useRoomPortal();
  useRoomOverlay(open);
  if (!catalog.artifacts) return null;
  return <Dialog.Root open={open} onOpenChange={(next) => { if (!next) onClose(); }}>
    <Dialog.Portal container={container}>
      <Dialog.Overlay className="ah-layer ah-sheet__backdrop" />
      <Dialog.Content className="ah-layer ah-artifacts" aria-describedby="ah-artifacts-description" onCloseAutoFocus={(event) => { event.preventDefault(); returnFocus(); }}>
        <div className="ah-artifacts__head"><ArtifactGlyph /><div><Dialog.Title>Artifacts</Dialog.Title><Dialog.Description id="ah-artifacts-description">HTML reviews and designs from your agent sessions</Dialog.Description></div><Dialog.Close className="ah-artifacts__close" aria-label="Close artifacts">×</Dialog.Close></div>
        {open && <LibraryContents port={catalog.artifacts} agents={agents} projects={projects} onOpenSession={(entry) => { onClose(); onOpenSession(entry.publication.session_id, entry.publication.timeline_id); }} />}
      </Dialog.Content>
    </Dialog.Portal>
  </Dialog.Root>;
}

function LibraryContents({ port, agents, projects, onOpenSession }: {
  readonly port: LibraryPort;
  readonly agents: readonly string[];
  readonly projects: readonly { id: string; name: string }[];
  readonly onOpenSession: (entry: ArtifactEntry) => void;
}) {
  const [q, setQ] = useState("");
  const [agent, setAgent] = useState("");
  const [project, setProject] = useState("");
  const [history, setHistory] = useState<ArtifactEntry | null>(null);
  const [selected, setSelected] = useState<ArtifactEntry | null>(null);
  const cache = useMemo(() => new ArtifactContentCache(), [port]);
  const selectedButton = useRef<HTMLButtonElement | null>(null);
  const request: ArtifactListRequest = { q: history ? null : q || null, agent: history ? null : agent || null, project: history ? null : project || null, artifact_id: history?.publication.artifact_id ?? null, limit: 100, cursor: null };
  const feed = useArtifacts(port, request);
  const filterKey = JSON.stringify(request);
  useEffect(() => { setSelected(null); }, [filterKey]);
  return <>
    <div className="ah-artifacts__filters">
      {history ? <><button type="button" onClick={() => setHistory(null)}>← All artifacts</button><strong>{history.publication.title} · Presentations</strong></> : <>
        <input type="search" aria-label="Search artifacts" placeholder="Search artifacts…" value={q} onChange={(event) => setQ(event.target.value)} />
        <select aria-label="Filter by agent" value={agent} onChange={(event) => setAgent(event.target.value)}><option value="">All agents</option>{agents.map((kind) => <option key={kind} value={kind}>{agentLabel(kind)}</option>)}</select>
        <select aria-label="Filter by project" value={project} onChange={(event) => setProject(event.target.value)}><option value="">All projects</option>{projects.map((item) => <option key={item.id} value={item.id}>{item.name}</option>)}</select>
      </>}
      <button type="button" onClick={() => { setSelected(null); feed.refresh(); }} disabled={feed.loading}>Refresh</button>
    </div>
    {feed.page && !feed.page.canonical_ready && <p className="ah-artifacts__message" role="status">Ingestion is still in progress. More artifacts may appear.</p>}
    {feed.error && <p className="ah-artifacts__message" role="alert">{feed.error}</p>}
    <div className="ah-artifacts__body">
      <div className="ah-artifacts__list" hidden={selected !== null} aria-label={history ? "Artifact presentations" : "Artifact list"} aria-busy={feed.loading}>
        {feed.page?.entries.map((entry) => <div key={entry.publication.id} className="ah-artifacts__item" data-selected={selected?.publication.id === entry.publication.id || undefined}>
          <button className="ah-artifacts__select" type="button" onClick={(event) => { selectedButton.current = event.currentTarget; setSelected(entry); }} aria-label={`Preview ${entry.publication.title}`}>
            <ArtifactThumbnail entry={entry} port={port} active={selected === null} />
            <span className="ah-artifacts__itemtitle"><ArtifactGlyph /><strong>{entry.publication.title}</strong></span>
            <span className="ah-artifacts__filename">{entry.publication.filename}</span>
            <span className="ah-artifacts__itemmeta"><AgentMark kind={entry.agent} />{entry.project_name ?? agentLabel(entry.agent)}</span>
            <span className="ah-artifacts__itemmeta">{history ? when(entry.publication.published_at) : entry.recovered_from ? "Recovered copy" : entry.availability !== "captured" && publishedArtifactUrl(entry.publication.url) ? "Published online" : AVAILABILITY[entry.availability]}</span>
          </button>
          <div className="ah-artifacts__cardactions">
            <button type="button" onClick={() => onOpenSession(entry)}>Open session ↗</button>
            {publishedArtifactUrl(entry.publication.url) && <a href={publishedArtifactUrl(entry.publication.url)!} target="_blank" rel="noopener noreferrer">Published link ↗</a>}
            {!history && entry.presentations > 1 && <button className="ah-artifacts__history" type="button" onClick={() => setHistory(entry)}>{entry.presentations} presentations →</button>}
          </div>
        </div>)}
        {feed.loading && <p className="ah-artifacts__message" role="status">Loading all artifacts…{feed.page?.entries.length ? ` ${feed.page.entries.length} found` : ""}</p>}
        {!feed.loading && !feed.error && feed.page?.entries.length === 0 && <div className="ah-artifacts__empty"><h3>{q || agent || project ? "No matching artifacts" : "No artifacts yet"}</h3><p>{q || agent || project ? "Try another search or filter." : "HTML explicitly presented by an agent will appear here as sessions are ingested."}</p></div>}
      </div>
      {selected && <ArtifactPreview key={selected.publication.id} entry={selected} port={port} cache={cache} onSession={() => onOpenSession(selected)} onBack={() => { setSelected(null); requestAnimationFrame(() => selectedButton.current?.focus()); }} />}
    </div>
  </>;
}
