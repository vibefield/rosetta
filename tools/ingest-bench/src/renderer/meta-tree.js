(function exposeMetaTree(scope, factory) {
  const api = factory();
  if (typeof module === 'object' && module.exports) module.exports = api;
  else scope.RosettaMetaTree = api;
}(typeof globalThis === 'undefined' ? window : globalThis, () => {
  function sessionTime(session) {
    const updated = Number(session.updatedAt);
    if (Number.isFinite(updated)) return updated;
    const started = Number(session.startedAt);
    return Number.isFinite(started) ? started : 0;
  }

  function compareNodes(left, right) {
    const byTime = sessionTime(right.session) - sessionTime(left.session);
    if (byTime) return byTime;
    return String(left.session.id).localeCompare(String(right.session.id));
  }

  function wouldCreateCycle(node, parent, nodesById) {
    const seen = new Set();
    let cursor = parent;
    while (cursor && !seen.has(cursor.session.id)) {
      if (cursor === node) return true;
      seen.add(cursor.session.id);
      cursor = nodesById.get(cursor.session.parentSessionId);
    }
    return false;
  }

  function assignDescendantCounts(node) {
    let count = 0;
    for (const child of node.children) count += 1 + assignDescendantCounts(child);
    node.descendantCount = count;
    return count;
  }

  function sortTree(nodes) {
    nodes.sort(compareNodes);
    for (const node of nodes) sortTree(node.children);
  }

  function visibleNode(node, matches, inheritedMatch = false) {
    const selfMatch = Boolean(matches(node.session));
    const includeDescendants = inheritedMatch || selfMatch;
    const children = node.children
      .map((child) => visibleNode(child, matches, includeDescendants))
      .filter(Boolean);
    if (!includeDescendants && !children.length) return null;
    return {
      session: node.session,
      children,
      childCount: node.children.length,
      descendantCount: node.descendantCount,
      detachedParent: node.detachedParent,
      selfMatch,
      hasMatchingDescendant: !selfMatch && !inheritedMatch && children.length > 0,
    };
  }

  function buildSessionForest(sessions, matches = () => true) {
    const nodesById = new Map(sessions.map((session) => [session.id, {
      session,
      children: [],
      detachedParent: false,
      descendantCount: 0,
    }]));
    const roots = [];
    for (const node of nodesById.values()) {
      const parentId = node.session.parentSessionId;
      const parent = parentId ? nodesById.get(parentId) : null;
      if (parent && parent !== node && !wouldCreateCycle(node, parent, nodesById)) {
        parent.children.push(node);
      } else {
        node.detachedParent = Boolean(parentId);
        roots.push(node);
      }
    }
    sortTree(roots);
    for (const root of roots) assignDescendantCounts(root);
    const visibleRoots = roots.map((root) => visibleNode(root, matches)).filter(Boolean);
    return {
      roots: visibleRoots,
      total: sessions.length,
      matchingCount: sessions.filter(matches).length,
      rootCount: roots.length,
      nestedCount: sessions.length - roots.length,
      detachedCount: visibleRoots.filter((root) => root.detachedParent).length,
    };
  }

  return { buildSessionForest };
}));
