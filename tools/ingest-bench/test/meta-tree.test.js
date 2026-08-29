const test = require('node:test');
const assert = require('node:assert/strict');

const { buildSessionForest } = require('../src/renderer/meta-tree');

function session(id, parentSessionId, updatedAt, title = id) {
  return { id, parentSessionId, updatedAt, title };
}

test('session forest nests descendants beneath their parent and sorts newest first', () => {
  const forest = buildSessionForest([
    session('older-main', null, 100),
    session('newer-main', null, 400),
    session('child', 'newer-main', 300),
    session('grandchild', 'child', 200),
  ]);

  assert.deepEqual(forest.roots.map((node) => node.session.id), ['newer-main', 'older-main']);
  assert.equal(forest.rootCount, 2);
  assert.equal(forest.nestedCount, 2);
  assert.equal(forest.roots[0].descendantCount, 2);
  assert.equal(forest.roots[0].children[0].session.id, 'child');
  assert.equal(forest.roots[0].children[0].children[0].session.id, 'grandchild');
});

test('session forest retains and marks ancestor paths for filtered descendants', () => {
  const forest = buildSessionForest([
    session('main', null, 300, 'Parent'),
    session('child', 'main', 200, 'Worker'),
    session('grandchild', 'child', 100, 'Needle'),
  ], (item) => item.title === 'Needle');

  assert.equal(forest.matchingCount, 1);
  assert.equal(forest.roots.length, 1);
  assert.equal(forest.roots[0].hasMatchingDescendant, true);
  assert.equal(forest.roots[0].children[0].hasMatchingDescendant, true);
  assert.equal(forest.roots[0].children[0].children[0].selfMatch, true);
});

test('session forest keeps a child with an unavailable parent as a detached root', () => {
  const forest = buildSessionForest([
    session('outside-child', 'parent-in-another-project', 100),
  ]);

  assert.equal(forest.rootCount, 1);
  assert.equal(forest.detachedCount, 1);
  assert.equal(forest.roots[0].detachedParent, true);
});

test('session forest only counts detached roots visible through the filter', () => {
  const forest = buildSessionForest([
    session('main', null, 300, 'Needle'),
    session('outside-child', 'parent-in-another-project', 100, 'Haystack'),
  ], (item) => item.title === 'Needle');

  assert.equal(forest.roots.length, 1);
  assert.equal(forest.detachedCount, 0);
});
