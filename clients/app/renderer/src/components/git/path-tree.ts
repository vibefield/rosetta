// Paths → a subtree, which is the shape the design record asks for:
// "Each group is a *subtree*, not a filtered list — a folder survives when
// anything inside it survives — so a path a reader learned in one view still
// holds in the other."
//
// Pure, and deliberately not mille: mille walks a real directory, and these
// rows are a set of paths git named. They wear the same geometry so the two
// columns read as one repository, and they do not pretend to be the same
// component.

/** A directory whose only child is another directory is drawn as one row —
 * `crates/store/src` rather than three rows deep. The design record refuses
 * this for column ③ (a walked tree's "only one child" is a claim about the
 * walk); here it is safe, because the set is closed and git named all of it. */
export interface TreeRow {
  readonly kind: "directory" | "file";
  /** What the row shows: a file's name, or a folder's collapsed path. */
  readonly label: string;
  /** Full workspace-relative path. Directories carry theirs too, so a folder
   * can be acted on as the set of files beneath it. */
  readonly path: string;
  readonly depth: number;
}

interface Node {
  readonly children: Map<string, Node>;
  readonly files: string[];
}

const node = (): Node => ({ children: new Map(), files: [] });

/**
 * Build the subtree over `paths`.
 *
 * Directories sort before files and both sort naturally, which is the order
 * mille draws column ③ in — two lists of the same repository that disagreed
 * about order would be two lists.
 */
export function buildPathTree(paths: readonly string[]): readonly TreeRow[] {
  const root = node();
  for (const path of paths) {
    const segments = path.split("/").filter((s) => s !== "");
    if (segments.length === 0) continue;
    let current = root;
    for (const segment of segments.slice(0, -1)) {
      let next = current.children.get(segment);
      if (next === undefined) {
        next = node();
        current.children.set(segment, next);
      }
      current = next;
    }
    current.files.push(segments[segments.length - 1]!);
  }

  const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: "base" });
  const rows: TreeRow[] = [];

  const walk = (current: Node, prefix: string, depth: number): void => {
    const directories = [...current.children.entries()].sort((a, b) =>
      collator.compare(a[0], b[0]),
    );
    for (const [name, child] of directories) {
      // Collapse a chain of single-child directories into one row.
      let label = name;
      let path = prefix === "" ? name : `${prefix}/${name}`;
      let inner = child;
      while (inner.files.length === 0 && inner.children.size === 1) {
        const [onlyName, onlyChild] = [...inner.children.entries()][0]!;
        label = `${label}/${onlyName}`;
        path = `${path}/${onlyName}`;
        inner = onlyChild;
      }
      rows.push({ kind: "directory", label, path, depth });
      walk(inner, path, depth + 1);
    }
    for (const name of [...current.files].sort(collator.compare)) {
      rows.push({
        kind: "file",
        label: name,
        path: prefix === "" ? name : `${prefix}/${name}`,
        depth,
      });
    }
  };

  walk(root, "", 0);
  return rows;
}

/** Every file at or beneath `row` — what acting on a folder row acts on. */
export function filesUnder(rows: readonly TreeRow[], row: TreeRow): readonly string[] {
  if (row.kind === "file") return [row.path];
  const prefix = `${row.path}/`;
  return rows
    .filter((candidate) => candidate.kind === "file" && candidate.path.startsWith(prefix))
    .map((candidate) => candidate.path);
}
