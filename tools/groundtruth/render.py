"""Render canonical ground-truth documents as self-contained HTML reports."""

from __future__ import annotations

import argparse
import html
from pathlib import Path
from typing import Any

from .model import GroundTruthError, read_json

CSS = """
:root{--bg:#fff;--fg:#1a1a1f;--mut:#667;--line:#e2e2ea;--card:#f7f7fa;--chip:#eef0ff;--chipfg:#3a41c4;--fix:#0a7d43;--obs:#8a5a00;--res:#8a2be2}
@media (prefers-color-scheme:dark){:root{--bg:#101014;--fg:#e8e8ee;--mut:#99a;--line:#2a2a33;--card:#17171d;--chip:#23244a;--chipfg:#aab2ff;--fix:#3fce87;--obs:#e5b45b;--res:#c08aff}}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--fg);font:14px/1.5 ui-monospace,SFMono-Regular,Menlo,monospace}
main{max-width:1100px;margin:0 auto;padding:2rem 1.5rem}h1{font-size:1.4rem}
h2{font-size:1.15rem;border-bottom:1px solid var(--line);padding-bottom:.4rem;margin-top:2.2rem}
h3{font-size:1rem;margin-top:1.8rem}h4.facet{font-size:.9rem;margin:1.2rem 0 .2rem;color:var(--mut)}.ver{color:var(--mut);font-weight:400}
.meta,.notes{color:var(--mut);font-size:.85rem}.glob{color:var(--mut);font-weight:400;font-size:.85rem}
table{border-collapse:collapse;width:100%;margin:.6rem 0}th{text-align:left;font-size:.75rem;text-transform:uppercase;letter-spacing:.05em;color:var(--mut);padding:.3rem .5rem}
td{padding:.3rem .5rem;border-top:1px solid var(--line);vertical-align:top}td.nested{padding:0 0 0 .5rem}.fields{background:var(--card);border-radius:6px;overflow:hidden}
.fname{font-weight:600}.obs{color:var(--mut);font-size:.85rem;text-align:right}.chip{background:var(--chip);color:var(--chipfg);border-radius:4px;padding:.05rem .4rem;font-size:.85rem}.req{color:var(--fix)}
.badge{border-radius:4px;padding:.05rem .45rem;font-size:.75rem;text-transform:lowercase}.ev-fixture{color:var(--fix);border:1px solid var(--fix)}.ev-observed{color:var(--obs);border:1px solid var(--obs)}.ev-research{color:var(--res);border:1px solid var(--res)}
details{margin:.25rem 0}summary{cursor:pointer;padding:.35rem .5rem}details.rtype{background:var(--card);border:1px solid var(--line);border-radius:6px;padding:.15rem .4rem;margin:.5rem 0}
.rname{font-weight:700}.variant,.arr{color:var(--mut);font-size:.85rem;margin:.3rem .5rem}code{background:var(--card);padding:.05rem .3rem;border-radius:4px}a{color:var(--chipfg);text-decoration:none}a:hover{text-decoration:underline}
""".strip()


def esc(value: Any) -> str:
    return html.escape(str(value), quote=True)


def inline_label(shape: dict[str, Any]) -> str | None:
    kind = shape.get("kind")
    if kind in {"null", "boolean", "number"}:
        return kind
    if kind == "string":
        values = shape.get("values")
        if values is None:
            return "string"
        pairs = sorted(values.items(), key=lambda pair: (-pair[1], pair[0]))
        shown = " | ".join(f"{esc(key)}({count})" for key, count in pairs[:8])
        more = f" +{len(pairs) - 8}" if len(pairs) > 8 else ""
        return f"enum: {shown}{more}"
    if kind == "unknown":
        return "?"
    if kind == "array":
        if "items" not in shape:
            return "array"
        inner = inline_label(shape["items"])
        return f"array&lt;{inner}&gt;" if inner is not None else None
    if kind == "any_of":
        labels = [inline_label(variant) for variant in shape.get("variants", [])]
        return " | ".join(labels) if all(label is not None for label in labels) else None
    return None


def render_shape(shape: dict[str, Any], depth: int = 0) -> str:
    kind = shape.get("kind")
    if kind == "object":
        return render_fields(shape.get("fields", {}), depth)
    if kind == "array" and "items" in shape:
        return f"<div class='arr'>array of:</div>{render_shape(shape['items'], depth)}"
    if kind == "any_of":
        parts: list[str] = []
        for index, variant in enumerate(shape.get("variants", []), start=1):
            label = inline_label(variant)
            if label is not None:
                parts.append(
                    f"<div class='variant'>variant {index}: <span class='chip'>{label}</span></div>"
                )
            else:
                parts.append(
                    f"<div class='variant'>variant {index}:</div>{render_shape(variant, depth)}"
                )
        return "".join(parts)
    return f"<span class='chip'>{inline_label(shape) or '?'}</span>"


def render_fields(fields: dict[str, dict[str, Any]], depth: int) -> str:
    rows = [
        "<table class='fields'><thead><tr><th>field</th><th>type</th><th>req</th><th>obs</th></tr></thead><tbody>"
    ]
    for name, field in fields.items():
        required = "<span class='req'>✓</span>" if field.get("required") else ""
        observed = esc(field.get("observed", ""))
        label = inline_label(field["shape"])
        if label is not None:
            rows.append(
                f"<tr><td class='fname'>{esc(name)}</td><td><span class='chip'>{label}</span></td>"
                f"<td>{required}</td><td class='obs'>{observed}</td></tr>"
            )
            continue
        nested = render_shape(field["shape"], depth + 1)
        kind = "array&lt;object&gt;" if field["shape"].get("kind") == "array" else "object"
        opened = " open" if depth == 0 else ""
        rows.append(
            f"<tr><td colspan='4' class='nested'><details{opened}><summary>"
            f"<span class='fname'>{esc(name)}</span> <span class='chip'>{kind}</span> "
            f"{required} <span class='obs'>{observed}</span></summary>{nested}</details></td></tr>"
        )
    rows.append("</tbody></table>")
    return "".join(rows)


def _badge(evidence: str) -> str:
    return f"<span class='badge ev-{esc(evidence)}'>{esc(evidence)}</span>"


def render(docs: list[dict[str, Any]]) -> str:
    body = [
        "<!doctype html><html><head><meta charset='utf-8'>",
        "<meta name='viewport' content='width=device-width,initial-scale=1'>",
        f"<title>Rosetta ground truth</title><style>{CSS}</style></head><body><main>",
        "<h1>rosetta ground truth</h1>",
    ]
    for doc in docs:
        captured = f" · captured {esc(doc['captured_at'])}" if doc.get("captured_at") else ""
        body.append(
            f"<section class='doc'><h2>{esc(doc['agent'])} "
            f"<span class='ver'>{esc(doc['agent_version'])}</span> {_badge(doc['evidence'])}</h2>"
            f"<p class='meta'>root <code>{esc(doc['data_root'])}</code> · generator "
            f"{esc(doc['generator'])}{captured}</p>"
        )
        if doc.get("sources"):
            sources = " · ".join(
                f"{esc(source['generator'])} ({esc(source['agent_version'])}, {esc(source['evidence'])})"
                for source in doc["sources"]
            )
            body.append(f"<p class='meta'>canonical — merged from: {sources}</p>")

        body.append(
            "<table class='layout'><thead><tr><th>stream</th><th>glob</th><th>format</th>"
            "<th>discipline</th><th>evidence</th><th>types</th><th>records</th></tr></thead><tbody>"
        )
        for stream in doc.get("streams", []):
            records = sum(item.get("observed", 0) for item in stream.get("record_types", {}).values())
            anchor = esc(f"{doc['agent_version']}-{stream['id']}")
            body.append(
                f"<tr><td class='fname'><a href='#{anchor}'>{esc(stream['id'])}</a></td>"
                f"<td><code>{esc(stream['glob'])}</code></td><td>{esc(stream['file_format'])}</td>"
                f"<td>{esc(stream['discipline'])}</td><td>{_badge(stream['evidence'])}</td>"
                f"<td>{len(stream.get('record_types', {}))}</td><td class='obs'>{records}</td></tr>"
            )
        body.append("</tbody></table>")

        for stream in doc.get("streams", []):
            if not stream.get("record_types"):
                continue
            anchor = esc(f"{doc['agent_version']}-{stream['id']}")
            body.append(
                f"<h3 id='{anchor}'>{esc(stream['id'])} "
                f"<code class='glob'>{esc(stream['glob'])}</code></h3>"
            )
            if stream.get("notes"):
                body.append(f"<p class='notes'>{esc(stream['notes'])}</p>")
            for record_type, record in stream.get("record_types", {}).items():
                title = record_type or "(document)"
                evidence = f" {_badge(record['evidence'])}" if record.get("evidence") else ""
                body.append(
                    f"<details class='rtype'><summary><span class='rname'>{esc(title)}</span>"
                    f"{evidence} <span class='obs'>×{record['observed']}</span></summary>"
                    f"{render_shape(record['shape'])}</details>"
                )
            for facet in stream.get("facets", []):
                body.append(
                    f"<h4 class='facet'>facet: {esc(facet['id'])} <span class='meta'>"
                    f"by {esc(facet['discriminator'])} — {esc(facet['path'])}</span></h4>"
                )
                for key, record in facet.get("record_types", {}).items():
                    evidence = f" {_badge(record['evidence'])}" if record.get("evidence") else ""
                    body.append(
                        f"<details class='rtype'><summary><span class='rname'>{esc(key)}</span>"
                        f"{evidence} <span class='obs'>×{record['observed']}</span></summary>"
                        f"{render_shape(record['shape'])}</details>"
                    )
        body.append("</section>")
    body.append("</main></body></html>\n")
    return "".join(body)


def render_files(inputs: list[Path], output: Path) -> None:
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(render([read_json(path) for path in inputs]))


def render_agent(agent_dir: Path) -> Path:
    output = agent_dir / "groundtruth.html"
    render_files([agent_dir / "groundtruth.json"], output)
    return output


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("inputs", nargs="+", type=Path)
    parser.add_argument("-o", "--output", type=Path)
    args = parser.parse_args(argv)
    output = args.output
    if output is None:
        if len(args.inputs) != 1:
            parser.error("-o is required when rendering multiple documents")
        output = args.inputs[0].with_suffix(".html")
    try:
        render_files(args.inputs, output)
    except GroundTruthError as error:
        parser.error(str(error))
    print(f"wrote {output}")
    return 0
