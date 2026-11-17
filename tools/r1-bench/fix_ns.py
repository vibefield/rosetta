#!/usr/bin/env python3
"""E4: fix the stress worker count N_s from BE's P0 runs (brief v3 §12 E4), before pair S.

r1_M4 = sum JSONL payload bytes / pool busy s  (capture-pool busyMs; JSONL bytes from snapshot.json byDriver json-lines)
W_M4  = sum applied payload bytes / writer busy s  ("payload bytes processed"; streamed-writer wallMs - idleMs)
N_s   = clamp(round(1.9 * W_M4 / r1_M4), 3, 9)
Per run, then the median of each rate. Also records BE's T_meta and finalize + publication medians for the
simulator. usage: python3 -I fix_ns.py SNAPSHOT_DIR OUT_JSON RUN_DIR...
"""
import json
import os
import statistics
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import summarize as SM  # noqa: E402

snap, out, *runs = sys.argv[1:]
meta = json.load(open(os.path.join(snap, 'snapshot.json')))
jsonl = sum(a['byDriver'].get('json-lines', {}).get('bytes', 0) for a in meta['agents'].values())
rows = []
for d in runs:
    r = SM.summarize(d)
    busy = r['capturePool']['busyMs'] / 1000.0
    w = r['writer']
    wbusy = (w['wallMs'] - w['idleMs']) / 1000.0
    applied = r['profile']['payload bytes processed']
    rows.append({'run': r['run'], 'poolBusyS': round(busy, 4), 'writerBusyS': round(wbusy, 4), 'r1': jsonl / busy,
                 'W': applied / wbusy, 'T_meta': r['T_meta'],
                 'finalizePublicationS': (r['profile']['bulk finalization time'] + r['profile']['publication time']) / 1000.0})
r1 = statistics.median(x['r1'] for x in rows)
W = statistics.median(x['W'] for x in rows)
ns = max(3, min(9, round(1.9 * W / r1)))
doc = {'format': 'r1-ns/1', 'rule': 'N_s = clamp(round(1.9 * W_M4 / r1_M4), 3, 9), medians over BE P0 runs',
       'jsonlPayloadBytes': jsonl, 'appliedPayloadBytes': rows[0] and SM.summarize(runs[0])['profile']['payload bytes processed'],
       'r1_M4': r1, 'W_M4': W, 'ratio_1_9_W_over_r1': 1.9 * W / r1, 'N_s': ns,
       'T_meta_median': statistics.median(x['T_meta'] for x in rows),
       'finalizePublication_median_s': statistics.median(x['finalizePublicationS'] for x in rows),
       'runs': [{k: (round(v, 1) if k in ('r1', 'W') else v) for k, v in x.items()} for x in rows]}
with open(out, 'x') as fh:
    json.dump(doc, fh, indent=1)
    fh.write('\n')
print(json.dumps({k: doc[k] for k in ('r1_M4', 'W_M4', 'ratio_1_9_W_over_r1', 'N_s', 'T_meta_median', 'finalizePublication_median_s')}))
