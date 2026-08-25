"""Convert a JSON Schema (draft) object into a rosetta shape document."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from . import FORMAT
from .model import GroundTruthError, write_json

GENERATOR = "tools.groundtruth json_schema 1"
MAX_DEPTH = 6


def _resolve(node: Any, defs: dict[str, Any], stack: tuple[str, ...]) -> tuple[Any, tuple[str, ...]]:
    if not isinstance(node, dict):
        return node, stack
    ref = node.get("$ref")
    if not isinstance(ref, str) or not ref.startswith("#/definitions/"):
        return node, stack
    name = ref.split("/", 2)[-1]
    if name in stack:
        return {"type": "object"}, stack
    target = defs.get(name)
    if not isinstance(target, dict):
        return {"type": "unknown"}, stack
    extra = {key: value for key, value in node.items() if key != "$ref"}
    merged = {**target, **extra} if extra else target
    return merged, (*stack, name)


def schema_shape(node: Any, defs: dict[str, Any], depth: int = 0, stack: tuple[str, ...] = ()) -> dict[str, Any]:
    """Best-effort structural shape for one JSON Schema node."""

    node, stack = _resolve(node, defs, stack)
    if not isinstance(node, dict):
        return {"kind": "unknown"}
    if depth >= MAX_DEPTH:
        return {"kind": "unknown"}

    if "anyOf" in node or "oneOf" in node:
        variants: list[dict[str, Any]] = []
        for option in node.get("anyOf") or node.get("oneOf") or []:
            incoming = schema_shape(option, defs, depth + 1, stack)
            if incoming not in variants:
                variants.append(incoming)
        if not variants:
            return {"kind": "unknown"}
        if len(variants) == 1:
            return variants[0]
        return {"kind": "any_of", "variants": variants}

    types = node.get("type")
    if isinstance(types, list):
        variants = [
            schema_shape({**node, "type": item}, defs, depth + 1, stack) for item in types
        ]
        unique: list[dict[str, Any]] = []
        for variant in variants:
            if variant not in unique:
                unique.append(variant)
        if len(unique) == 1:
            return unique[0]
        return {"kind": "any_of", "variants": unique}

    if types == "null" or node.get("type") == "null":
        return {"kind": "null"}
    if types == "boolean":
        return {"kind": "boolean"}
    if types in {"integer", "number"}:
        return {"kind": "number"}
    if types == "string":
        return {"kind": "string"}
    if types == "array":
        shape: dict[str, Any] = {"kind": "array"}
        if "items" in node:
            shape["items"] = schema_shape(node["items"], defs, depth + 1, stack)
        return shape
    if types == "object" or "properties" in node:
        required = set(node.get("required") or [])
        fields: dict[str, dict[str, Any]] = {}
        properties = node.get("properties") if isinstance(node.get("properties"), dict) else {}
        for key, spec in properties.items():
            fields[str(key)] = {
                "shape": schema_shape(spec, defs, depth + 1, stack),
                "required": key in required,
            }
        result: dict[str, Any] = {"kind": "object"}
        if fields:
            result["fields"] = dict(sorted(fields.items()))
        return result
    return {"kind": "unknown"}


def _event_type_const(event: dict[str, Any]) -> str | None:
    props = event.get("properties") if isinstance(event.get("properties"), dict) else {}
    type_spec = props.get("type") if isinstance(props.get("type"), dict) else {}
    const = type_spec.get("const")
    return const if isinstance(const, str) else None


def persisted_session_events(schema: dict[str, Any]) -> dict[str, dict[str, Any]]:
    """Map persisted event type → envelope+data shape. Ephemeral events omitted."""

    defs = schema.get("definitions")
    if not isinstance(defs, dict):
        raise GroundTruthError("session-events schema: missing definitions")
    session_event = defs.get("SessionEvent")
    if not isinstance(session_event, dict) or not isinstance(session_event.get("anyOf"), list):
        raise GroundTruthError("session-events schema: SessionEvent.anyOf missing")

    records: dict[str, dict[str, Any]] = {}
    for item in session_event["anyOf"]:
        node, _ = _resolve(item if isinstance(item, dict) else {}, defs, ())
        if not isinstance(node, dict):
            continue
        rtype = _event_type_const(node)
        if rtype is None:
            continue
        required = node.get("required") if isinstance(node.get("required"), list) else []
        if "ephemeral" in required:
            continue
        records[rtype] = {"observed": 0, "shape": schema_shape(node, defs)}
    if not records:
        raise GroundTruthError("session-events schema: no persisted event types")
    return dict(sorted(records.items()))


def session_events_writer_doc(
    schema: dict[str, Any], *, agent: str, version: str
) -> dict[str, Any]:
    records = persisted_session_events(schema)
    return {
        "format": FORMAT,
        "agent": agent,
        "agent_version": version,
        "evidence": "writer",
        "generator": GENERATOR,
        "data_root": "~/.copilot",
        "streams": [
            {
                "id": "events",
                "glob": "session-state/*/events.jsonl",
                "file_format": "jsonl",
                "discipline": "append_only",
                "evidence": "writer",
                "notes": (
                    "Writer JSON Schema for persisted session events (ephemeral "
                    "streaming types omitted). Field requiredness is the schema "
                    "contract; fixtures prove what the CLI actually writes."
                ),
                "discriminator": "type",
                "record_types": records,
            }
        ],
    }


def write_copilot_writer_schema(install: Path, agent_dir: Path, version: str) -> Path:
    matches = sorted(
        install.glob("node_modules/@github/copilot-*/schemas/session-events.schema.json")
    )
    if not matches:
        raise GroundTruthError(
            "copilot session-events.schema.json not found under the installed npm package"
        )
    schema_path = matches[0]
    try:
        schema = json.loads(schema_path.read_text())
    except (OSError, json.JSONDecodeError) as error:
        raise GroundTruthError(f"{schema_path}: {error}") from error
    vendor = agent_dir / "sources" / "vendor"
    vendor.mkdir(parents=True, exist_ok=True)
    dest = vendor / f"{version}.session-events.schema.json"
    dest.write_text(schema_path.read_text())
    document = session_events_writer_doc(schema, agent=agent_dir.name, version=version)
    out = agent_dir / "sources" / f"{version}.writer-schema.json"
    write_json(out, document)
    return out
