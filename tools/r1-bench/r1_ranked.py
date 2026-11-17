#!/usr/bin/env python3
"""Ranked-result digest of one run's published projection (brief v3 §12 E3
"Search"; the imgdefer steps4-7 query_digest.py method, 20dc85f precedent).

Warm-starts `rosettad serve` on the run's OWN db (pair_r1.sh calls this after
the digest SQL and ledger verify, so the warm start's search sidecar and
catch-up touch nothing already measured), waits for `ready`, polls
/api/status until search is ready, runs the 10 product-diagnostic-v3 cases as
/api/search?q=..&limit=200, SHA-256s the ordered hits (json sort_keys,
compact) and the ordered (timeline_id, session_id, kind, at) ids, and stops
with SIGINT. ranked_ids_sha256 is the gate (natural keys only, U-2 safe);
ranked_sha256 (every hit field) is reported.

usage: python3 -I r1_ranked.py BINARY DB SNAPSHOT_HOME OUT_JSON [AGENT ...]
"""
import hashlib
import http.client
import json
import os
import signal
import subprocess
import sys
import threading
import time
import urllib.parse

HERE = os.path.dirname(os.path.abspath(__file__))
WORKLOAD = os.path.join(HERE, 'product-diagnostic-v3.json')
WORKLOAD_SHA = '12e38e5ecc9a0e3d89bdd15b4c9fcc429defd07325f685cb108e91a97465aee7'


def sha(b):
    return hashlib.sha256(b).hexdigest()


def main():
    binary, db, home, out_path, *agents = sys.argv[1:]
    agents = agents or ['claude-code', 'codex']
    raw = open(WORKLOAD, 'rb').read()
    assert sha(raw) == WORKLOAD_SHA, 'workload changed'
    cases = json.loads(raw)['cases']
    args = [binary, 'serve', '--db', db, '--home', home, *sum((['--agent', a] for a in agents), []),
            '--addr', '127.0.0.1:0', '--json']
    t0 = time.monotonic()
    proc = subprocess.Popen(args, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, bufsize=1)
    ready = {}
    got = threading.Event()

    def pump():
        for line in proc.stdout:
            if line.startswith('{') and not got.is_set():
                try:
                    row = json.loads(line)
                except ValueError:
                    continue
                if row.get('event') == 'ready':
                    ready['origin'] = row['origin']
                    ready['at'] = time.monotonic() - t0
                    got.set()
    threading.Thread(target=pump, daemon=True).start()
    result = {'format': 'r1-ranked/1', 'workload_sha256': WORKLOAD_SHA}
    try:
        if not got.wait(300):
            raise RuntimeError('warm start never reported ready')
        u = urllib.parse.urlsplit(ready['origin'])

        def get(path):
            c = http.client.HTTPConnection(u.hostname, u.port, timeout=600)
            c.request('GET', path)
            r = c.getresponse()
            body = r.read()
            c.close()
            return r.status, body
        deadline = time.monotonic() + 900
        polls = 0
        while True:
            status, body = get('/api/status')
            polls += 1
            if status == 200 and json.loads(body).get('search', {}).get('readiness') == 'ready':
                break
            if time.monotonic() > deadline or proc.poll() is not None:
                raise RuntimeError('search never became ready')
            time.sleep(0.05)
        result['search_ready_s'] = round(time.monotonic() - t0, 3)
        result['status_polls'] = polls
        results = []
        for case in cases:
            params = {'q': case['query'], 'limit': 200}
            params.update(case.get('filter', {}))
            status, body = get('/api/search?' + urllib.parse.urlencode(params))
            assert status == 200, (case['label'], status)
            value = json.loads(body)
            assert value['state']['readiness'] == 'ready', case['label']
            results.append({'label': case['label'], 'hits': value['hits']})
        full = json.dumps(results, sort_keys=True, separators=(',', ':')).encode()
        ids = json.dumps([{'label': r['label'], 'hits': [[x.get('timeline_id'), x.get('session_id'), x.get('kind'), x.get('at')]
                                                         for x in r['hits']]} for r in results],
                         sort_keys=True, separators=(',', ':')).encode()
        result.update({
            'ranked_sha256': sha(full), 'ranked_ids_sha256': sha(ids),
            'per_case': [{'label': r['label'], 'hits': len(r['hits']),
                          'ids_sha256': sha(json.dumps([[x.get('timeline_id'), x.get('session_id'), x.get('kind'), x.get('at')]
                                                        for x in r['hits']], separators=(',', ':')).encode())}
                         for r in results],
        })
    finally:
        if proc.poll() is None:
            proc.send_signal(signal.SIGINT)
            try:
                proc.wait(120)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait()
        result['warm_exit'] = proc.returncode
        result['warm_wall_s'] = round(time.monotonic() - t0, 3)
        with open(out_path, 'x') as fh:
            json.dump(result, fh, indent=1, sort_keys=True)
            fh.write('\n')
    print(json.dumps({k: result.get(k) for k in ('ranked_ids_sha256', 'search_ready_s', 'warm_exit')}))


if __name__ == '__main__':
    main()
