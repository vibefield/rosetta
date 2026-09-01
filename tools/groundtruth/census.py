"""Capture a privacy-bounded structural census of an organic agent data tree."""

from __future__ import annotations

import argparse
import json
import os
import sqlite3
from collections import defaultdict
from pathlib import Path
from typing import Any

from . import FORMAT
from .facets import extract as extract_facets
from .farm import _is_sqlite_database, _quote_identifier, _sqlite_shape
from .model import (
    GroundTruthError,
    ShapeAccumulator,
    build_canonical,
    enumerable_value,
    jsonl_record_type,
    load_layout,
    matching_stream,
    repo_root,
    sanitize_census_document,
    scrub_shape,
    sqlite_schema_name,
    write_json,
)
from .render import render_agent

GENERATOR = "tools.groundtruth census 2"
SHAPE_MERGE_CAP = 256
DOCUMENT_SHAPE_MERGE_CAP = 4096
FACET_MERGE_CAP = 32


def _shape_sample(value: Any) -> Any:
    """Keep object keys and JSON kinds; drop payloads so organic censuses finish.

    Grok tool results store command output as huge strings and byte arrays.
    Those values must never land in the committed census anyway.
    """

    if isinstance(value, str):
        return "" if not enumerable_value(value) else value
    if isinstance(value, list):
        if not value:
            return value
        return [_shape_sample(value[0])]
    if isinstance(value, dict):
        return {key: _shape_sample(item) for key, item in value.items()}
    return value

# Deliberately excludes config, credentials, state, and free-text streams. The
# schema inference itself is privacy-bounded, but parsing fewer sensitive files
# is a stronger guarantee than relying on scrubbing after the fact.
CENSUS_STREAMS = {
    "claude-code": {
        "transcript",
        "subagent-transcript",
        "subagent-metadata",
        "prompt-history",
        "todos",
        "tasks",
        "team-config",
        "team-inbox",
        "session-meta",
    },
    # Session JSON/JSONL only. Skips auth, config, logs, system prompts, and
    # terminal captures — those are free text or credentials.
    "grok": {
        "updates",
        "chat-history",
        "events",
        "summary",
        "signals",
        "rewind-points",
        "prompt-context",
        "prompt-history",
        "plan",
        "feedback",
        "subagents",
    },
    "antigravity": {
        "transcript",
        "transcript-full",
        "transcript-chunks",
        "transcript-full-chunks",
        "history",
        "conversation-db",
        "conversation-summaries",
    },
    # Session JSON/JSONL only. Skips credentials, oauth, config, logs, blobs,
    # tool-result bodies, user-history prompts, telemetry, and derived indexes.
    "kimi-code": {
        "wire",
        "session-state",
        "session-index",
        "workspaces",
        "workspace-trust",
        "migrations-effort",
    },
}


def _default_root(agent: str) -> Path:
    if agent == "claude-code":
        return Path(os.environ.get("CLAUDE_CONFIG_DIR", Path.home() / ".claude"))
    if agent == "grok":
        return Path(os.environ.get("GROK_HOME", Path.home() / ".grok"))
    if agent == "antigravity":
        return Path.home() / ".gemini" / "antigravity-cli"
    if agent == "kimi-code":
        return Path(os.environ.get("KIMI_CODE_HOME", Path.home() / ".kimi-code"))
    raise GroundTruthError(f"no default data root for {agent}")


def _absorb_sqlite_schema(
    accumulator: ShapeAccumulator, database_name: str, path: Path
) -> int:
    """Table/column shapes and row counts only — never row payloads."""

    uri = f"{path.resolve().as_uri()}?mode=ro"
    connection = sqlite3.connect(uri, uri=True)
    try:
        tables = [
            row[0]
            for row in connection.execute(
                "SELECT name FROM sqlite_master "
                "WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name"
            )
        ]
        records = 0
        for table in tables:
            columns = connection.execute(
                f"PRAGMA table_info({_quote_identifier(table)})"
            ).fetchall()
            rows = connection.execute(
                f"SELECT count(*) FROM {_quote_identifier(table)}"
            ).fetchone()[0]
            records += rows
            fields = {
                name: {
                    "shape": _sqlite_shape(declared_type),
                    "required": bool(not_null or primary_key),
                }
                for _, name, declared_type, not_null, _, primary_key in columns
            }
            accumulator.absorb_doc(
                f"{database_name}:{table}",
                {
                    "observed": rows,
                    "shape": {"kind": "object", "fields": dict(sorted(fields.items()))},
                },
            )
        return records
    except sqlite3.Error as error:
        raise GroundTruthError(f"introspecting {path}: {error}") from error
    finally:
        connection.close()


def capture(
    agent_dir: Path,
    data_root: Path,
    label: str,
    writer_version: str | None = None,
) -> tuple[dict[str, Any], int]:
    agent = agent_dir.name
    if agent not in CENSUS_STREAMS:
        raise GroundTruthError(f"organic census is not configured for {agent}")
    layout = load_layout(agent_dir)
    if agent == "claude-code" and not (data_root / "projects").is_dir():
        raise GroundTruthError(f"no projects/ under {data_root}; nothing to census")
    if agent == "grok" and not (data_root / "sessions").is_dir():
        raise GroundTruthError(f"no sessions/ under {data_root}; nothing to census")
    if agent == "antigravity" and not data_root.is_dir():
        raise GroundTruthError(f"no CLI data root at {data_root}; nothing to census")
    if agent == "kimi-code" and not (data_root / "sessions").is_dir():
        raise GroundTruthError(f"no sessions/ under {data_root}; nothing to census")

    accumulators: dict[str, ShapeAccumulator] = defaultdict(ShapeAccumulator)
    facets: dict[tuple[str, str], tuple[str, str, ShapeAccumulator]] = {}
    merge_counts: dict[tuple[str, str], int] = defaultdict(int)
    facet_merge_counts: dict[tuple[str, str, str], int] = defaultdict(int)
    records = 0
    walk_roots = [data_root / "sessions"] if agent == "grok" else [data_root]
    for walk_root in walk_roots:
        if not walk_root.is_dir():
            continue
        for path in sorted(walk_root.rglob("*")):
            if not path.is_file():
                continue
            relative = path.relative_to(data_root).as_posix()
            stream = matching_stream(layout, relative)
            if stream is None or stream["id"] not in CENSUS_STREAMS[agent]:
                continue
            if stream["file_format"] == "sqlite":
                if not _is_sqlite_database(path):
                    continue
                records += _absorb_sqlite_schema(
                    accumulators[stream["id"]],
                    sqlite_schema_name(path, relative),
                    path,
                )
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

            # Claude data files from many installed writer versions can
            # coexist indefinitely. When a file carries a version stamp, a
            # scoped census must not blend its schema into the current pin.
            if (
                writer_version is not None
                and agent == "claude-code"
                and stream["id"]
                in {"transcript", "subagent-transcript", "session-meta"}
            ):
                versions = {
                    value["version"]
                    for value in values
                    if isinstance(value, dict) and isinstance(value.get("version"), str)
                }
                if writer_version not in versions:
                    continue
                if versions != {writer_version}:
                    values = [
                        value
                        for value in values
                        if isinstance(value, dict) and value.get("version") == writer_version
                    ]

            context: dict[str, str] = {}
            for value in values:
                if not isinstance(value, (dict, list)):
                    continue
                records += 1
                discriminant = ""
                if stream["file_format"] == "jsonl" and stream["id"] not in {
                    "session-index",
                    "user-history",
                }:
                    discriminant = jsonl_record_type(value)
                cap_key = (stream["id"], discriminant)
                merge_counts[cap_key] += 1
                shape_merge_cap = (
                    DOCUMENT_SHAPE_MERGE_CAP
                    if stream["file_format"] == "json"
                    else SHAPE_MERGE_CAP
                )
                if merge_counts[cap_key] <= shape_merge_cap:
                    accumulators[stream["id"]].absorb(
                        discriminant, _shape_sample(value)
                    )
                elif discriminant in accumulators[stream["id"]].types:
                    accumulators[stream["id"]].types[discriminant]["observed"] += 1
                if isinstance(value, dict):
                    for hit in extract_facets(agent, stream["id"], value, context):
                        key = (stream["id"], hit["facet_id"])
                        if key not in facets:
                            facets[key] = (
                                hit["path"],
                                hit["discriminator"],
                                ShapeAccumulator(),
                            )
                        facet_key = (stream["id"], hit["facet_id"], hit["key"])
                        facet_merge_counts[facet_key] += 1
                        accumulator = facets[key][2]
                        if facet_merge_counts[facet_key] <= FACET_MERGE_CAP:
                            accumulator.absorb(hit["key"], _shape_sample(hit["value"]))
                        elif hit["key"] in accumulator.types:
                            accumulator.types[hit["key"]]["observed"] += 1

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
            "notes": (
                "Structure-only census of a live organic dataset. "
                "Shapes sampled from the first "
                f"{DOCUMENT_SHAPE_MERGE_CAP if spec['file_format'] == 'json' else SHAPE_MERGE_CAP} "
                "records per type; "
                f"nested facet shapes from the first {FACET_MERGE_CAP} records per "
                "nested discriminator; observed counts cover the full tree."
            ),
            "record_types": accumulator.into_types(),
        }
        if spec["file_format"] == "jsonl" and any(stream.get("record_types", {})):
            if spec["id"] == "updates":
                stream["discriminator"] = "sessionUpdate"
            elif spec["id"] not in {
                "prompt-history",
                "rewind-points",
                "unified-log",
                "history",
                "session-index",
                "user-history",
            }:
                stream["discriminator"] = "type"
        elif spec["file_format"] == "sqlite" and stream.get("record_types"):
            stream["discriminator"] = "table"
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
        "agent_version": writer_version or label,
        "evidence": "observed",
        "generator": GENERATOR,
        "data_root": layout["data_root"],
        "streams": streams,
    }
    if label.startswith("observed-"):
        document["captured_at"] = label.removeprefix("observed-")
    if writer_version is not None:
        document["writer_version"] = writer_version
    document = sanitize_census_document(document)
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
    parser.add_argument(
        "--writer-version",
        help="include only transcript records written by this agent version",
    )
    args = parser.parse_args(argv)
    try:
        agent_dir = repo_root() / "groundtruth" / args.agent
        document, records = capture(
            agent_dir,
            args.root or _default_root(args.agent),
            args.label,
            args.writer_version,
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
