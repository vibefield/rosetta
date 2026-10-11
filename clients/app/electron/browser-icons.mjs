// Read installed application artwork locally; no network icon service.
import { execFile } from "node:child_process";
import { readFile, stat } from "node:fs/promises";
import path from "node:path";
import { promisify } from "node:util";
import { app, nativeImage } from "electron";

const execute = promisify(execFile);
const LINUX_ICONS = {
  chrome: ["google-chrome", "google-chrome-stable"],
  firefox: ["firefox"],
  edge: ["microsoft-edge", "microsoft-edge-stable"],
  brave: ["brave-browser", "brave"],
  opera: ["opera"],
  vivaldi: ["vivaldi", "vivaldi-stable"],
  zen: ["zen-browser", "zen"],
  chromium: ["chromium", "chromium-browser"],
  "firefox-developer": ["firefox-developer-edition"],
};

export async function installedBrowserIcon(target, id) {
  if (process.platform === "darwin") {
    // getFileIcon(.app) can return the generic application icon. Quick Look
    // decodes the bundle's actual ICNS artwork, including Retina renditions.
    const { stdout } = await execute("/usr/bin/plutil", ["-extract", "CFBundleIconFile", "raw", "-o", "-", path.join(target, "Contents/Info.plist")], { timeout: 2000, maxBuffer: 4096 });
    const name = stdout.trim();
    if (!name || path.basename(name) !== name) return null;
    const iconPath = path.join(target, "Contents/Resources", path.extname(name) ? name : `${name}.icns`);
    if (!(await stat(iconPath)).isFile()) return null;
    const icon = await nativeImage.createThumbnailFromPath(iconPath, { width: 40, height: 40 });
    return icon.isEmpty() ? null : icon.toDataURL();
  }
  if (process.platform === "linux") {
    // Executables often have a generic file icon on Linux too. Browser
    // packages install their artwork into the freedesktop hicolor theme.
    const roots = [process.env.XDG_DATA_HOME ?? path.join(app.getPath("home"), ".local/share"),
      ...(process.env.XDG_DATA_DIRS ?? "/usr/local/share:/usr/share").split(":")].filter(root => path.isAbsolute(root));
    for (const name of LINUX_ICONS[id] ?? [id]) {
      for (const root of roots) {
        for (const size of ["48x48", "32x32", "64x64", "128x128", "256x256", "scalable", null]) {
          for (const extension of ["png", "svg"]) {
            const candidate = size === null ? path.join(root, "pixmaps", `${name}.${extension}`)
              : path.join(root, "icons/hicolor", size, "apps", `${name}.${extension}`);
            try {
              const info = await stat(candidate);
              if (!info.isFile() || info.size > 256 * 1024) continue;
              const bytes = await readFile(candidate);
              return `data:image/${extension === "svg" ? "svg+xml" : "png"};base64,${bytes.toString("base64")}`;
            } catch { /* try the next installed rendition */ }
          }
        }
      }
    }
    return null;
  }
  const icon = await app.getFileIcon(target, { size: "normal" });
  return icon.isEmpty() ? null : icon.toDataURL();
}
