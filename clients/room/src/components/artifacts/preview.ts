// Captured pages run only in an opaque-origin iframe. This CSP is prepended
// before any captured markup; subsequent policies can only restrict it.
export const ARTIFACT_SANDBOX = "allow-scripts";
export const ARTIFACT_MAX_BYTES = 16 * 1024 * 1024;

export function publishedArtifactUrl(value: string | null): string | null {
  if (!value || /[\u0000-\u0020]/.test(value)) return null;
  try {
    const url = new URL(value);
    return url.protocol === "https:" && url.hostname && !url.username && !url.password ? url.href : null;
  } catch { return null; }
}

export function artifactDocument(html: string): string {
  const policy = [
    "default-src 'none'",
    "script-src 'unsafe-inline' 'unsafe-eval' https://cdn.jsdelivr.net https://cdnjs.cloudflare.com https://unpkg.com",
    "style-src 'unsafe-inline' https://fonts.googleapis.com https://cdn.jsdelivr.net",
    "font-src data: https://fonts.gstatic.com",
    "img-src data: blob:",
    "media-src data: blob:",
    "connect-src 'none'",
    "frame-src 'none'",
    "object-src 'none'",
    "worker-src 'none'",
    "base-uri https://artifact.invalid",
    "form-action 'none'",
  ].join("; ");
  // Full documents and the fragments used by presentation tools both parse in
  // this shell. Base defaults are intentionally modest: authored styles win.
  return `<!doctype html><html><head><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="${policy}"><meta name="referrer" content="no-referrer"><base href="https://artifact.invalid/"><style>
    :root { color-scheme: light; --color-background-primary: #fff; --color-background-secondary: #f5f5f3; --color-background-tertiary: #eeede9; --color-text-primary: #252521; --color-text-secondary: #62625c; --color-text-tertiary: #85857e; --color-border-primary: #d5d5ce; --color-border-secondary: #e5e5df; --color-border-tertiary: #efefea; --color-background-info: #eaf1ff; --color-text-info: #2e5b99; --color-background-success: #edf6e8; --color-text-success: #3c6530; --color-background-warning: #fff3d6; --color-text-warning: #855c12; --color-background-danger: #fdecea; --color-text-danger: #9a3830; --border-radius-md: 8px; --border-radius-lg: 12px; --font-sans: system-ui,sans-serif; --font-mono: ui-monospace,monospace; }
    html { background: #fff; color: #252521; font-family: var(--font-sans); } body { margin: 24px; } * { box-sizing: border-box; } button,input,select,textarea { font: inherit; } a { color: #315e9c; }
  </style></head><body>${html}</body></html>`;
}
