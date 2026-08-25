"""Capture a privacy-bounded structural census of an organic agent data tree."""

from __future__ import annotations

import argparse
import json
import os
from collections import defaultdict
from pathlib import Path
from typing import Any

from . import FORMAT
from .facets import extract as extract_facets
from .model import (
    GroundTruthError,
    ShapeAccumulator,
    build_canonical,
    load_layout,
    matching_stream,
    repo_root,
    scrub_shape,
    write_json,
)
from .render import render_agent

GENERATOR = "tools.groundtruth census 1"

# Deliberately excludes config, credentials, state, and free-text streams. The
# schema inference itself is privacy-bounded, but parsing fewer sensitive files
# is a stronger guarantee than relying on scrubbing after the fact.
CENSUS_STREAMS = {
    "claude-code": {
        "transcript",
        "subagent-transcript",
        "prompt-history",
        "todos",
        "tasks",
        "team-config",
        "team-inbox",
        "session-meta",
    }
}


def _default_root(agent: str) -> Path:
    if agent == "claude-code":
        return Path(os.environ.get("CLAUDE_CONFIG_DIR", Path.home() / ".claude"))
    raise GroundTruthError(f"no default data root for {agent}")


def capture(agent_dir: Path, data_root: Path, label: str) -> tuple[dict[str, Any], int]:
    agent = agent_dir.name
    if agent not in CENSUS_STREAMS:
        raise GroundTruthError(f"organic census is not configured for {agent}")
    layout = load_layout(agent_dir)
    if not (data_root / "projects").is_dir():
        raise GroundTruthError(f"no projects/ under {data_root}; nothing to census")

    accumulators: dict[str, ShapeAccumulator] = defaultdict(ShapeAccumulator)
    facets: dict[tuple[str, str], tuple[str, str, ShapeAccumulator]] = {}
    records = 0
    for path in sorted(data_root.rglob("*")):
        if not path.is_file():
            continue
        relative = path.relative_to(data_root).as_posix()
        stream = matching_stream(layout, relative)
        if stream is None or stream["id"] not in CENSUS_STREAMS[agent]:
            continue
        if stream["file_format"] not in {"json", "jsonl"}:
            continue
        try:
            text = path.read_text()
        except (OSError, UnicodeDecodeError):
            continue
        values: list[Any] = []
        if stream["file_format"] == "jsonl":
            for line in text.splitlines():
                if not line.strip():
                    continue
                try:
                    values.append(json.loads(line))
                except json.JSONDecodeError:
                    continue
        else:
            try:
                values.append(json.loads(text))
            except json.JSONDecodeError:
                continue

        context: dict[str, str] = {}
        for value in values:
            if not isinstance(value, (dict, list)):
                continue
            records += 1
            discriminant = ""
            if stream["file_format"] == "jsonl":
                discriminant = (
                    value.get("type", "<untyped>")
                    if isinstance(value, dict)
                    else "<untyped>"
                )
            accumulators[stream["id"]].absorb(discriminant, value)
            if isinstance(value, dict):
                for hit in extract_facets(agent, stream["id"], value, context):
                    key = (stream["id"], hit["facet_id"])
                    if key not in facets:
                        facets[key] = (
                            hit["path"],
                            hit["discriminator"],
                            ShapeAccumulator(),
                        )
                    facets[key][2].absorb(hit["key"], hit["value"])

    streams: list[dict[str, Any]] = []
    for spec in layout["streams"]:
        accumulator = accumulators.get(spec["id"])
        if accumulator is None:
            continue
        stream: dict[str, Any] = {
            "id": spec["id"],
            "glob": spec["glob"],
            "file_format": spec["file_format"],
            "discipline": spec["discipline"],
            "evidence": "observed",
            "notes": "Structure-only census of a live organic dataset.",
            "record_types": accumulator.into_types(),
        }
        if spec["file_format"] == "jsonl" and spec["id"] != "prompt-history":
            stream["discriminator"] = "type"
        stream_facets = []
        for (stream_id, facet_id), (path, discriminator, facet_accumulator) in sorted(
            facets.items()
        ):
            if stream_id == spec["id"]:
                stream_facets.append(
                    {
                        "id": facet_id,
                        "path": path,
                        "discriminator": discriminator,
                        "record_types": facet_accumulator.into_types(),
                    }
                )
        if stream_facets:
            stream["facets"] = stream_facets
        streams.append(stream)

    document = {
        "format": FORMAT,
        "agent": agent,
        "agent_version": label,
        "evidence": "observed",
        "generator": GENERATOR,
        "data_root": layout["data_root"],
        "streams": streams,
    }
    scrub = [os.environ.get("USER", "")]
    scrub.extend(
        token.strip() for token in os.environ.get("GT_CENSUS_SCRUB", "").split(",")
    )
    for stream in document["streams"]:
        for record_type in stream.get("record_types", {}).values():
            scrub_shape(record_type["shape"], scrub)
        for facet in stream.get("facets", []):
            for record_type in facet.get("record_types", {}).values():
                scrub_shape(record_type["shape"], scrub)
    return document, records


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("agent", choices=sorted(CENSUS_STREAMS))
    parser.add_argument("output", type=Path)
    parser.add_argument("--label", required=True)
    parser.add_argument("--root", type=Path)
    args = parser.parse_args(argv)
    try:
        agent_dir = repo_root() / "groundtruth" / args.agent
        document, records = capture(
            agent_dir, args.root or _default_root(args.agent), args.label
        )
        write_json(args.output, document)
        print(
            f"records={records} streams={len(document['streams'])} "
            f"facets={sum(len(stream.get('facets', [])) for stream in document['streams'])} "
            f"-> {args.output}"
        )
        if args.output.resolve().parent == (agent_dir / "sources").resolve():
            canonical = build_canonical(agent_dir)
            report = render_agent(agent_dir)
            print(f"canonical ground truth -> {canonical}")
            print(f"schema report -> {report}")
    except GroundTruthError as error:
        parser.error(str(error))
    return 0
