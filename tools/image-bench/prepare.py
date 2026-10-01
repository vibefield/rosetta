from pathlib import Path
import argparse, base64, hashlib, json
from PIL import Image, ImageDraw

parser = argparse.ArgumentParser(description='Create and freeze 20 synthetic Codex image sessions from five public/synthetic research PNGs. Requires Pillow.')
parser.add_argument('work', type=Path)
parser.add_argument('sources', type=Path, help='Directory containing photo/ui/text-dark/text-light/alpha.png; see research/img/rosetta-2026-10-01')
args = parser.parse_args()
root = args.work.resolve()
root.mkdir(parents=True, exist_ok=True)
research = args.sources.resolve()
if (root / 'source-home').exists():
    raise SystemExit('Refusing to replace an existing frozen corpus; choose a new work directory.')
home = root / 'source-home'
sources = home / '.codex/sessions/2026/10/01'
sources.mkdir(parents=True, exist_ok=True)
for index, name in enumerate(('photo', 'ui', 'text-dark', 'text-light', 'alpha')):
    for revision in range(4):
        image = Image.open(research / (name + '.png')).convert('RGBA')
        ImageDraw.Draw(image).rectangle((10, 10, 90, 45), fill=(50 + revision * 40, 80 + index * 25, 125, 255))
        path = root / f'input-{index}-{revision}.png'
        image.save(path, compress_level=6)
        b64 = base64.b64encode(path.read_bytes()).decode()
        sid = f'image-pipeline-{index}-{revision}'
        header = {'timestamp': '2026-10-01T00:00:00Z', 'type': 'session_meta', 'payload': {'id': sid, 'timestamp': '2026-10-01T00:00:00Z', 'cwd': '/fixture/image-pipeline', 'originator': 'codex_exec', 'cli_version': '0.150.1', 'source': 'cli', 'model_provider': 'openai'}}
        message = {'timestamp': '2026-10-01T00:00:01Z', 'type': 'response_item', 'payload': {'type': 'message', 'role': 'user', 'content': [{'type': 'input_text', 'text': f'Review the {name} image fixture {revision}. Check screenshot color and error handling.'}, {'type': 'input_image', 'image_url': 'data:image/png;base64,' + b64}]}}
        reply = {'timestamp': '2026-10-01T00:00:02Z', 'type': 'response_item', 'payload': {'type': 'message', 'role': 'assistant', 'content': [{'type': 'output_text', 'text': f'The {name} fixture includes image pipeline validation and stable search results.'}]}}
        (sources / f'rollout-{sid}.jsonl').write_text('\n'.join(json.dumps(row) for row in (header, message, reply)) + '\n')
queries = ['image', 'screenshot', 'color', 'error', '"stable search"', 'photo', 'text-dark', 'alpha']
(root / 'queries.json').write_text(json.dumps(queries, indent=2))
checks = {str(p.relative_to(root)): hashlib.sha256(p.read_bytes()).hexdigest() for p in sorted(home.rglob('*')) if p.is_file()}
checks['queries.json'] = hashlib.sha256((root / 'queries.json').read_bytes()).hexdigest()
(root / 'source-sha256.json').write_text(json.dumps(checks, indent=2))
for p in home.rglob('*'):
    p.chmod(0o555 if p.is_dir() else 0o444)
home.chmod(0o555)
(root / 'queries.json').chmod(0o444)
print('Frozen', len(checks) - 1, 'source files and query suite')
