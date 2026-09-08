import path from "node:path";
import { pathToFileURL } from "node:url";

export const APP_SCHEME = "rosetta";
export const APP_ORIGIN = "rosetta://app";
export const APP_PRIVILEGES = {
  standard: true,
  secure: true,
  supportFetchAPI: true,
  corsEnabled: true,
};

/** Resolve only files in the renderer build, even for encoded traversal URLs. */
export function rendererFileUrl(requestUrl, directory) {
  try {
    const url = new URL(requestUrl);
    if (
      url.protocol !== `${APP_SCHEME}:` ||
      url.hostname !== "app" ||
      url.port ||
      url.username ||
      url.password
    )
      return null;
    const pathname = decodeURIComponent(url.pathname === "/" ? "/index.html" : url.pathname);
    if (pathname.includes("\0") || pathname.includes("\\")) return null;
    const root = path.resolve(directory);
    const file = path.resolve(root, `.${pathname}`);
    const relative = path.relative(root, file);
    if (
      !relative ||
      relative.startsWith(`..${path.sep}`) ||
      relative === ".." ||
      path.isAbsolute(relative)
    )
      return null;
    return pathToFileURL(file).href;
  } catch {
    return null;
  }
}
