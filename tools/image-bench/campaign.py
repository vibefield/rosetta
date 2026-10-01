#!/usr/bin/env python3
"""Paired image pipeline benchmark against one frozen synthetic source home.

Inputs: WORK/source-home, WORK/queries.json, WORK/source-sha256.json,
WORK/bin/{before,after}. Creates new run directories; never uses a real home.
"""
import argparse
import hashlib
import json
import platform
import queue
import resource
import sqlite3
import statistics
import subprocess
import threading
import time
from pathlib import Path
from urllib.parse import urlencode
from urllib.request import urlopen


def digest(value):
    return hashlib.sha256(value).hexdigest()


def freeze_check(root):
    manifest = json.loads((root / 'source-sha256.json').read_text())
    for relative, checksum in manifest.items():
        path = root / relative
        assert path.stat().st_mode & 0o222 == 0, f'writable benchmark input: {path}'
        assert digest(path.read_bytes()) == checksum, f'changed benchmark input: {path}'
    return digest((root / 'source-sha256.json').read_bytes())


def inspect(db):
    conn = sqlite3.connect(f'file:{db}?mode=ro', uri=True)
    counts = {table: conn.execute(f'SELECT COUNT(*) FROM {table}').fetchone()[0]
              for table in ('agent_runtime', 'project', 'session', 'message')}
    images = []
    for (detail,) in conn.execute('SELECT detail FROM message WHERE detail IS NOT NULL ORDER BY id'):
        for attachment in json.loads(detail).get('attachments', []):
            if attachment['source']['type'] == 'asset':
                images.append(attachment)
    conn.close()
    assets = Path(str(db) + '.assets')
    totals = {'disk_bytes': sum(p.stat().st_size for p in assets.iterdir() if p.is_file()),
              'files': sum(1 for p in assets.iterdir() if p.is_file()), 'images': len(images)}
    for variant in ('full', 'rail', 'tile', 'tile2x', 'preview'):
        totals[variant + '_bytes'] = sum(
            item['source']['asset'].get('image_variants', {}).get(variant, item['source']['asset'])['byte_len']
            for item in images)
    return counts, totals, images


def query(binary, db, home, queries, log):
    lines = queue.Queue()
    process = subprocess.Popen([str(binary), 'serve', '--db', str(db), '--home', str(home),
        '--agent', 'codex', '--addr', '127.0.0.1:0', '--json'], stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT, text=True, bufsize=1)
    def pump():
        with log.open('w') as output:
            for line in process.stdout:
                output.write(line); output.flush(); lines.put(line)
    worker = threading.Thread(target=pump, daemon=True); worker.start()
    try:
        deadline = time.monotonic() + 60
        origin = None
        while time.monotonic() < deadline:
            line = lines.get(timeout=max(0.1, deadline - time.monotonic()))
            if line.startswith('{'):
                row = json.loads(line)
                if row.get('event') == 'ready': origin = row['origin']; break
        assert origin, 'daemon did not announce ready'
        results = []
        for text in queries:
            for _ in range(400):
                with urlopen(origin + '/api/search?' + urlencode({'q': text, 'limit': 200})) as response:
                    value = json.load(response)
                if value['state']['readiness'] == 'ready': break
                time.sleep(.05)
            else: raise RuntimeError('search did not become ready')
            results.append({'q': text, 'hits': value['hits']})
        return results
    finally:
        process.terminate()
        try: process.wait(timeout=10)
        except subprocess.TimeoutExpired: process.kill(); process.wait()
        worker.join(timeout=2)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('work', type=Path)
    parser.add_argument('--id', default='paired')
    parser.add_argument('--repeats', type=int, default=3)
    args = parser.parse_args()
    root = args.work.resolve()
    checksum = freeze_check(root)
    out = root / args.id; out.mkdir()
    queries = json.loads((root / 'queries.json').read_text())
    result = {'campaign': args.id, 'machine': platform.platform(),
        'cpu': subprocess.check_output(['sysctl', '-n', 'machdep.cpu.brand_string'], text=True).strip(),
        'source_manifest_sha256': checksum, 'query_sha256': digest((root / 'queries.json').read_bytes()),
        'configuration': {'repeats': args.repeats, 'command': 'ingest --agent codex --profile --json',
            'format': 'lossy WebP Q80 method2 alpha100', 'sizes': [160, 320, 640, 1600]}, 'sides': {}}
    for side in ('before', 'after'):
        binary = root / 'bin' / side
        runs = []
        for repeat in range(args.repeats):
            directory = out / side / str(repeat); directory.mkdir(parents=True)
            db = directory / 'db.sqlite'
            cpu_before = resource.getrusage(resource.RUSAGE_CHILDREN)
            start = time.perf_counter()
            with (directory / 'ingest.log').open('w') as log:
                subprocess.run([str(binary), 'ingest', '--db', str(db), '--home', str(root / 'source-home'),
                    '--agent', 'codex', '--profile', '--json'], stdout=log, stderr=subprocess.STDOUT, check=True)
            elapsed = time.perf_counter() - start
            cpu_after = resource.getrusage(resource.RUSAGE_CHILDREN)
            cpu = (cpu_after.ru_utime + cpu_after.ru_stime) - (cpu_before.ru_utime + cpu_before.ru_stime)
            counts, assets, images = inspect(db)
            ranked = query(binary, db, root / 'source-home', queries, directory / 'serve.log')
            serialized = json.dumps(ranked, sort_keys=True, separators=(',', ':')).encode()
            (directory / 'ranked-results.json').write_bytes(serialized)
            (directory / 'images.json').write_text(json.dumps(images))
            run = {'seconds': elapsed, 'cpu_seconds': cpu, 'counts': counts, 'assets': assets, 'ranked_sha256': digest(serialized)}
            runs.append(run); print(side, repeat, json.dumps(run), flush=True)
        result['sides'][side] = {'binary_sha256': digest(binary.read_bytes()), 'runs': runs,
            'median_seconds': statistics.median(run['seconds'] for run in runs),
            'median_cpu_seconds': statistics.median(run['cpu_seconds'] for run in runs)}
    all_runs = [run for side in result['sides'].values() for run in side['runs']]
    assert all(run['counts'] == all_runs[0]['counts'] for run in all_runs), 'logical counts changed'
    assert all(run['ranked_sha256'] == all_runs[0]['ranked_sha256'] for run in all_runs), 'ranked results changed'
    assert freeze_check(root) == checksum
    result['equal_counts_and_ranked_results'] = True
    (out / 'summary.json').write_text(json.dumps(result, indent=2) + '\n')
    print(out / 'summary.json', flush=True)


if __name__ == '__main__':
    main()
