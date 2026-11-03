#!/usr/bin/env python3
"""E1.8 snapshot-structure.json from one B2 `--json --profile` run (brief v3 §12 E1 step 8).

Keeps structure only: each runtime's `walk-plan` event (per source in global
order: [path_rank, lane, tree_index, read_len, age_ms_from_newest]), its `walk:`
and `census:` line fields, the `stream-prepared` big_source_splits sizes, and a
per-tree roll-up (lane, sources, bytes, max source, newest age). No path, id,
runtime id, content or timing.

usage: python3 -I make_structure.py SNAPSHOT_DIR RUN_DIR OUT_JSON
"""
import json
import os
import re
import sys

snap, run, out = sys.argv[1:]
meta = json.load(open(os.path.join(snap, 'snapshot.json')))
report = json.load(open(os.path.join(run, 'report.json')))
pair = json.load(open(os.path.join(os.path.dirname(run.rstrip('/')), 'pair.json')))
LARGE = 64 << 20


def fields(line):
    f = dict(re.findall(r'(\w+)=(\S+)', line))
    f.pop('runtime', None)
    return {k: (int(v) if re.fullmatch(r'-?\d+', v) else v) for k, v in f.items()}


walk_lines, census = {}, {}
for t in report['textLines']:
    if t['line'].startswith('walk: '):
        f = fields(t['line'])
        walk_lines[f.pop('agent')] = f
    elif t['line'].startswith('census: '):
        f = fields(t['line'])
        census[f.pop('agent')] = f
plans = {}
for e in report['otherEvents']:
    if e.get('event') != 'walk-plan':
        continue
    trees = {}
    for path_rank, lane, tree, read_len, age in e['sources']:
        key = f'{lane}:{tree}'
        t = trees.setdefault(key, {'lane': lane, 'tree': tree, 'sources': 0, 'bytes': 0, 'maxSourceBytes': 0,
                                   'newestAgeMs': None})
        t['sources'] += 1
        t['bytes'] += read_len
        t['maxSourceBytes'] = max(t['maxSourceBytes'], read_len)
        t['newestAgeMs'] = age if t['newestAgeMs'] is None else min(t['newestAgeMs'], age)
    rollup = sorted(trees.values(), key=lambda t: (t['newestAgeMs'], t['tree'], t['lane']))
    plans[e['agent']] = {'mode': e['mode'], 'digest': e['digest'], 'sourceCount': len(e['sources']),
                         'sources': e['sources'], 'treesByLaneByRecency': rollup}
splits = {}
for s in report['stages']:
    if s.get('stage') == 'parallel-stream-prepare':
        splits[s.get('agent')] = s.get('bigSourceSplits')
b2 = next(s for s in pair['sides'] if s['tag'] == 'B2')
doc = {
    'format': 'r1-snapshot-structure/1',
    'method': 'brief v3 §12 E1 step 8: one B2 `serve --json --profile` run over the frozen snapshot; walk-plan events, '
              'walk: and census: lines, big_source_splits; structure only (no paths, ids, content or timings)',
    'binary': {'role': 'B2', 'commit': b2.get('commit'), 'sha256': report['binarySha256']},
    'snapshot': {'walkInputsDigest': meta['walkInputsDigest'], 'manifestSha256': meta['manifestSha256'],
                 'totals': meta['totals']},
    'privatePathsRecorded': False,
    'walkSourceTuple': ['path_rank', 'lane', 'tree_index', 'read_len', 'age_ms_from_newest'],
    'largeBytes': LARGE,
    'walk': walk_lines,
    'census': census,
    'walkPlan': plans,
    'bigSourceSplits': splits,
    'codexDryRunCheckD': {'groups_hoisted': (walk_lines.get('codex') or {}).get('groups_hoisted'),
                          'ok': ((walk_lines.get('codex') or {}).get('groups_hoisted') or 0) >= 1},
}
with open(out, 'x') as fh:
    json.dump(doc, fh, indent=1)
    fh.write('\n')
os.chmod(out, 0o444)
print(json.dumps({'walk': walk_lines, 'codexCheckD': doc['codexDryRunCheckD'],
                  'trees': {a: len(p['treesByLaneByRecency']) for a, p in plans.items()}}, indent=1))
