"""Ground-truth document model and structural schema inference.

This module intentionally uses only the Python standard library. Ground-truth
work is development-time orchestration and document transformation; keeping it
outside the Rust workspace makes the evidence/runtime boundary visible.
"""

from __future__ import annotations

import copy
import fnmatch
import hashlib
import json
import re
import tomllib
from collections.abc import Iterable, Mapping
from pathlib import Path
from typing import Any

from . import FORMAT, LAYOUT_FORMAT

ENUM_CARDINALITY_CAP = 128
ENUM_VALUE_BYTES_CAP = 96
DYNAMIC_KEY_CAP = 40
UUID_KEY = re.compile(
    r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$",
    re.IGNORECASE,
)

EVIDENCE_STRENGTH = {"research": 0, "observed": 1, "fixture": 2, "writer": 3}
FILE_FORMATS = {"jsonl", "json", "toml", "markdown", "sqlite", "text"}
DISCIPLINES = {"append_only", "rewrite_in_place", "database"}

# Inferred shapes keep values only when the field itself is enum-like. This
# preserves real protocol enums, model/provider names, and built-in tool names
# without publishing arbitrary prompts, paths, titles, filenames, or IDs.
CENSUS_ENUM_FIELDS = {
    "action",
    "agenttype",
    "api",
    "apibackend",
    "backend",
    "category",
    "channel",
    "direction",
    "effort",
    "entrypoint",
    "event",
    "fileformat",
    "finish",
    "finishreason",
    "kind",
    "language",
    "level",
    "mediatype",
    "method",
    "mimetype",
    "mode",
    "model",
    "modelid",
    "operation",
    "origin",
    "outcome",
    "permission",
    "permissionmode",
    "phase",
    "promptsource",
    "protocol",
    "protocolversion",
    "provider",
    "providerid",
    "reason",
    "role",
    "scope",
    "sessionkind",
    "sessionupdate",
    "severity",
    "source",
    "state",
    "status",
    "stopreason",
    "subtype",
    "tool",
    "toolname",
    "tools",
    "transport",
    "trigger",
    "type",
    "usertype",
    "version",
    "visibility",
}
SEMVER_VALUE = re.compile(r"^v?\d+(?:\.\d+){0,3}(?:[-+][A-Za-z0-9.-]+)?$")
OPAQUE_VALUE = re.compile(
    r"^(?:msg|tool|toolu|call|session|req|request|task|thread|run|wf)_[A-Za-z0-9-]*[0-9][A-Za-z0-9-]+$",
    re.IGNORECASE,
)
LONG_HEX_VALUE = re.compile(r"^[0-9a-f]{12,}$", re.IGNORECASE)

CENSUS_ENUM_SUFFIXES = {
    "action",
    "backend",
    "category",
    "channel",
    "direction",
    "event",
    "format",
    "kind",
    "language",
    "level",
    "method",
    "mode",
    "model",
    "operation",
    "phase",
    "protocol",
    "provider",
    "reason",
    "role",
    "scope",
    "source",
    "state",
    "status",
    "style",
    "subtype",
    "tier",
    "transport",
    "trigger",
    "type",
    "version",
    "visibility",
}


class GroundTruthError(ValueError):
    """A malformed layout or ground-truth document."""


def repo_root(start: Path | None = None) -> Path:
    current = (start or Path.cwd()).resolve()
    for candidate in (current, *current.parents):
        manifest = candidate / "Cargo.toml"
        if manifest.is_file() and "[workspace]" in manifest.read_text():
            return candidate
    raise GroundTruthError("run from inside the rosetta repository")


def read_json(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as error:
        raise GroundTruthError(f"{path}: {error}") from error
    if not isinstance(value, dict):
        raise GroundTruthError(f"{path}: expected a JSON object")
    return value


def write_json(path: Path, value: Mapping[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n")


def enumerable_value(value: str) -> bool:
    return bool(value) and len(value.encode()) <= ENUM_VALUE_BYTES_CAP and all(
        char.isascii() and (char.isalnum() or char in "-_.") for char in value
    )


def _normalized_field_name(field_name: str | None) -> str:
    return re.sub(r"[^a-z0-9]", "", (field_name or "").lower())


def _census_enum_field(field_name: str | None) -> bool:
    normalized = _normalized_field_name(field_name)
    return normalized in CENSUS_ENUM_FIELDS or any(
        normalized.endswith(suffix) for suffix in CENSUS_ENUM_SUFFIXES
    )


def _safe_enum_values(
    field_name: str | None, values: Mapping[str, Any]
) -> dict[str, int] | None:
    """Return publishable enum counts for an inferred string field.

    A short ASCII string is not automatically an enum: IDs, filenames, task
    titles, and snippets can all satisfy that test. The field name must also be
    protocol-enum-like. MCP names retain their class but not the configured
    server or user-defined tool name.
    """

    if not _census_enum_field(field_name):
        return None
    normalized = _normalized_field_name(field_name)
    safe: dict[str, int] = {}
    for raw_value, raw_count in values.items():
        value = str(raw_value)
        if normalized in {"tool", "toolname", "tools"} and value == "mcp__*":
            safe[value] = safe.get(value, 0) + int(raw_count)
            continue
        if not enumerable_value(value):
            continue
        if UUID_KEY.fullmatch(value) or OPAQUE_VALUE.fullmatch(value) or LONG_HEX_VALUE.fullmatch(value):
            continue
        if normalized.endswith("version") and not SEMVER_VALUE.fullmatch(value):
            continue
        if normalized in {"tool", "toolname", "tools"} and value.startswith("mcp__"):
            value = "mcp__*"
        safe[value] = safe.get(value, 0) + int(raw_count)
    return dict(sorted(safe.items())) or None


def value_like_key(key: str) -> bool:
    if len(key.encode()) > 48:
        return True
    if any(char.isspace() or char in "/\\" for char in key):
        return True
    if UUID_KEY.fullmatch(key):
        return True
    if len(key) > 2 and key.startswith("<") and key.endswith(">"):
        return True
    return False


def schema_field_key(key: str) -> bool:
    """Stable SQL/JSON field names, as opposed to maps keyed by values."""
    if not key or value_like_key(key):
        return False
    if key[0].isdigit():
        return False
    return all(char.isalnum() or char == "_" for char in key)


def sqlite_schema_name(path: Path, relative: str) -> str:
    """Stable schema-dump identity for UUID-named per-record databases."""

    if UUID_KEY.fullmatch(path.stem):
        parent = Path(relative).parent.as_posix()
        if parent in {"", "."}:
            return f"uuid{path.suffix}"
        return f"{parent}{path.suffix}"
    return path.name


def _field(shape: dict[str, Any], *, required: bool, observed: int | None) -> dict[str, Any]:
    value: dict[str, Any] = {"shape": shape, "required": required}
    if observed is not None:
        value["observed"] = observed
    return value


def _collapse_dynamic(fields: dict[str, dict[str, Any]]) -> dict[str, dict[str, Any]]:
    fields = dict(sorted(fields.items()))
    # Wide tables with real column names (Hermes sessions has 50+) must stay
    # named. Collapse only value-keyed maps (UUIDs, paths) or huge anonymous
    # objects.
    if not any(value_like_key(key) for key in fields) and (
        len(fields) <= DYNAMIC_KEY_CAP or all(schema_field_key(key) for key in fields)
    ):
        return fields

    merged: dict[str, Any] | None = None
    total = 0
    has_counts = True
    for field in fields.values():
        merged = copy.deepcopy(field["shape"]) if merged is None else merge_shapes(merged, field["shape"])
        if "observed" in field:
            total += field["observed"]
        else:
            has_counts = False
    return {
        "<dynamic>": _field(
            merged or {"kind": "unknown"},
            required=True,
            observed=total if has_counts else None,
        )
    }


def shape_of(value: Any, field_name: str | None = None) -> dict[str, Any]:
    if value is None:
        return {"kind": "null"}
    if isinstance(value, bool):
        return {"kind": "boolean"}
    if isinstance(value, (int, float)):
        return {"kind": "number"}
    if isinstance(value, str):
        shape: dict[str, Any] = {"kind": "string"}
        if enumerable_value(value):
            safe = _safe_enum_values(field_name, {value: 1})
            if safe is not None:
                shape["values"] = safe
        return shape
    if isinstance(value, list):
        items: dict[str, Any] | None = None
        for item in value:
            incoming = shape_of(item, field_name)
            items = incoming if items is None else merge_shapes(items, incoming)
        shape = {"kind": "array"}
        if items is not None:
            shape["items"] = items
        return shape
    if isinstance(value, dict):
        fields = {
            str(key): _field(shape_of(item, str(key)), required=True, observed=1)
            for key, item in value.items()
        }
        return {"kind": "object", "fields": _collapse_dynamic(fields)}
    return {"kind": "unknown"}


def _mergeable(left: dict[str, Any], right: dict[str, Any]) -> bool:
    return left == right or (
        left.get("kind") == right.get("kind") and left.get("kind") in {"object", "array"}
    )


def merge_shapes(left: dict[str, Any], right: dict[str, Any]) -> dict[str, Any]:
    """Merge two structural shapes without mutating either input."""

    left = copy.deepcopy(left)
    right = copy.deepcopy(right)
    left_kind = left.get("kind")
    right_kind = right.get("kind")

    if left_kind == "unknown":
        return right
    if right_kind == "unknown":
        return left

    if left_kind == right_kind == "object":
        left_fields = left.get("fields", {})
        right_fields = right.get("fields", {})
        fields: dict[str, dict[str, Any]] = {}
        for key in sorted(set(left_fields) | set(right_fields)):
            first = left_fields.get(key)
            second = right_fields.get(key)
            if first is None:
                incoming = copy.deepcopy(second)
                incoming["required"] = False
                fields[key] = incoming
            elif second is None:
                incoming = copy.deepcopy(first)
                incoming["required"] = False
                fields[key] = incoming
            else:
                observed = None
                if "observed" in first and "observed" in second:
                    observed = first["observed"] + second["observed"]
                fields[key] = _field(
                    merge_shapes(first["shape"], second["shape"]),
                    required=bool(first.get("required") and second.get("required")),
                    observed=observed,
                )
        return {"kind": "object", "fields": _collapse_dynamic(fields)}

    if left_kind == right_kind == "array":
        first = left.get("items")
        second = right.get("items")
        shape: dict[str, Any] = {"kind": "array"}
        if first is not None and second is not None:
            shape["items"] = merge_shapes(first, second)
        elif first is not None or second is not None:
            shape["items"] = copy.deepcopy(first if first is not None else second)
        return shape

    if left_kind == right_kind == "string":
        shape = {"kind": "string"}
        first = left.get("values")
        second = right.get("values")
        if first is not None and second is not None:
            values = dict(first)
            for key, count in second.items():
                values[key] = values.get(key, 0) + count
            if len(values) <= ENUM_CARDINALITY_CAP:
                shape["values"] = dict(sorted(values.items()))
        return shape

    if left == right:
        return left

    variants: list[dict[str, Any]] = []
    for shape in (left, right):
        if shape.get("kind") == "any_of":
            variants.extend(copy.deepcopy(shape.get("variants", [])))
        else:
            variants.append(shape)

    merged: list[dict[str, Any]] = []
    for variant in variants:
        for index, existing in enumerate(merged):
            if _mergeable(existing, variant):
                merged[index] = merge_shapes(existing, variant)
                break
        else:
            merged.append(variant)
    return merged[0] if len(merged) == 1 else {"kind": "any_of", "variants": merged}


class ShapeAccumulator:
    def __init__(self) -> None:
        self.types: dict[str, dict[str, Any]] = {}

    def absorb(self, discriminant: str, value: Any) -> None:
        self.absorb_doc(
            discriminant,
            {"observed": 1, "shape": shape_of(value)},
        )

    def absorb_doc(self, discriminant: str, record_type: Mapping[str, Any]) -> None:
        incoming = copy.deepcopy(dict(record_type))
        current = self.types.get(discriminant)
        if current is None:
            self.types[discriminant] = incoming
            return
        current["observed"] += incoming["observed"]
        current["shape"] = merge_shapes(current["shape"], incoming["shape"])

    def into_types(self) -> dict[str, dict[str, Any]]:
        return dict(sorted(self.types.items()))


def _merge_field_docs(
    first: Mapping[str, Any], second: Mapping[str, Any]
) -> dict[str, Any]:
    observed = None
    if "observed" in first and "observed" in second:
        observed = int(first["observed"]) + int(second["observed"])
    return _field(
        merge_shapes(dict(first["shape"]), dict(second["shape"])),
        required=bool(first.get("required") and second.get("required")),
        observed=observed,
    )


def sanitize_census_shape(
    shape: Mapping[str, Any], field_name: str | None = None
) -> dict[str, Any]:
    """Remove payload-like values and keys from an organic census shape."""

    result = copy.deepcopy(dict(shape))
    kind = result.get("kind")
    if kind == "string":
        values = result.get("values")
        if isinstance(values, Mapping):
            safe = _safe_enum_values(field_name, values)
            if safe is None:
                result.pop("values", None)
            else:
                result["values"] = safe
        return result
    if kind == "array":
        if isinstance(result.get("items"), Mapping):
            result["items"] = sanitize_census_shape(result["items"], field_name)
        return result
    if kind == "object":
        fields: dict[str, dict[str, Any]] = {}
        for key, raw_field in result.get("fields", {}).items():
            field = copy.deepcopy(dict(raw_field))
            field["shape"] = sanitize_census_shape(field["shape"], key)
            # JSON object keys may themselves be values (project names, file
            # names, IDs). Stable identifier-like schema fields stay named;
            # all others are represented structurally as one dynamic field.
            safe_key = key if key == "<dynamic>" or schema_field_key(key) else "<dynamic>"
            if safe_key in fields:
                fields[safe_key] = _merge_field_docs(fields[safe_key], field)
            else:
                fields[safe_key] = field
        result["fields"] = dict(sorted(fields.items()))
        return result
    if kind == "any_of":
        result["variants"] = [
            sanitize_census_shape(variant, field_name)
            for variant in result.get("variants", [])
        ]
    return result


def _sanitize_census_record_types(
    record_types: Mapping[str, Mapping[str, Any]],
    discriminator: str | None,
    *,
    tool_names: bool = False,
) -> dict[str, dict[str, Any]]:
    sanitized: dict[str, dict[str, Any]] = {}
    semantic_field = "tool" if tool_names else discriminator
    preserve_names = tool_names or discriminator == "table" or _census_enum_field(discriminator)
    sentinels = {"", "<none>", "<unnamed>", "<unmatched>", "<untyped>"}
    for raw_name, raw_record_type in record_types.items():
        name = raw_name
        if name not in sentinels:
            if discriminator == "table":
                name = raw_name
            else:
                safe = (
                    _safe_enum_values(semantic_field, {name: 1})
                    if preserve_names
                    else None
                )
                name = next(iter(safe)) if safe else "<dynamic>"
        record_type = copy.deepcopy(dict(raw_record_type))
        record_type["shape"] = sanitize_census_shape(record_type["shape"])
        current = sanitized.get(name)
        if current is None:
            sanitized[name] = record_type
        else:
            current["observed"] = int(current["observed"]) + int(record_type["observed"])
            current["shape"] = merge_shapes(current["shape"], record_type["shape"])
    return dict(sorted(sanitized.items()))


def sanitize_census_document(document: Mapping[str, Any]) -> dict[str, Any]:
    """Apply the committed organic-census publication policy."""

    result = copy.deepcopy(dict(document))
    result["generator"] = "tools.groundtruth census 2"
    for stream in result.get("streams", []):
        if stream.get("record_types"):
            stream["record_types"] = _sanitize_census_record_types(
                stream["record_types"], stream.get("discriminator")
            )
        for facet in stream.get("facets", []):
            if facet.get("record_types"):
                facet["record_types"] = _sanitize_census_record_types(
                    facet["record_types"],
                    facet.get("discriminator"),
                    tool_names="tool" in str(facet.get("id", "")).lower(),
                )
    return result


def _validate_stream(stream: Mapping[str, Any], source: Path) -> None:
    required = {"id", "glob", "file_format", "discipline", "evidence"}
    missing = required - stream.keys()
    if missing:
        raise GroundTruthError(f"{source}: stream missing {sorted(missing)}")
    if stream["file_format"] not in FILE_FORMATS:
        raise GroundTruthError(f"{source}: invalid file_format {stream['file_format']!r}")
    if stream["discipline"] not in DISCIPLINES:
        raise GroundTruthError(f"{source}: invalid discipline {stream['discipline']!r}")
    if stream["evidence"] not in EVIDENCE_STRENGTH:
        raise GroundTruthError(f"{source}: invalid evidence {stream['evidence']!r}")


def load_layout(agent_dir: Path) -> dict[str, Any]:
    path = agent_dir / "layout.toml"
    try:
        layout = tomllib.loads(path.read_text())
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise GroundTruthError(f"{path}: {error}") from error
    if layout.get("format") != LAYOUT_FORMAT:
        raise GroundTruthError(f"{path}: expected format {LAYOUT_FORMAT!r}")
    if layout.get("agent") != agent_dir.name:
        raise GroundTruthError(f"{path}: agent must match directory name {agent_dir.name!r}")
    streams = layout.get("streams")
    if not isinstance(streams, list):
        raise GroundTruthError(f"{path}: streams must be an array")
    seen: set[str] = set()
    for stream in streams:
        _validate_stream(stream, path)
        if stream["id"] in seen:
            raise GroundTruthError(f"{path}: duplicate stream {stream['id']!r}")
        seen.add(stream["id"])
    return layout


def glob_match(pattern: str, path: str) -> bool:
    pattern_parts = pattern.split("/")
    path_parts = path.split("/")

    def matches(pattern_index: int, path_index: int) -> bool:
        if pattern_index == len(pattern_parts):
            return path_index == len(path_parts)
        part = pattern_parts[pattern_index]
        if part == "**":
            return matches(pattern_index + 1, path_index) or (
                path_index < len(path_parts) and matches(pattern_index, path_index + 1)
            )
        return (
            path_index < len(path_parts)
            and fnmatch.fnmatchcase(path_parts[path_index], part)
            and matches(pattern_index + 1, path_index + 1)
        )

    return matches(0, 0)


def jsonl_record_type(value: Any) -> str:
    """Discriminant for one JSONL record. Agents disagree on the key."""

    if not isinstance(value, dict):
        return "<untyped>"
    if isinstance(value.get("type"), str) and value["type"]:
        return value["type"]
    if isinstance(value.get("event"), str) and value["event"]:
        return value["event"]
    params = value.get("params") if isinstance(value.get("params"), dict) else {}
    update = params.get("update") if isinstance(params.get("update"), dict) else {}
    if isinstance(update.get("sessionUpdate"), str) and update["sessionUpdate"]:
        return update["sessionUpdate"]
    if isinstance(value.get("sessionUpdate"), str) and value["sessionUpdate"]:
        return value["sessionUpdate"]
    if isinstance(value.get("method"), str) and value["method"]:
        return value["method"]
    if isinstance(value.get("role"), str) and value["role"]:
        return value["role"]
    return "<untyped>"


def matching_stream(layout: Mapping[str, Any], relative_path: str) -> dict[str, Any] | None:
    return next(
        (
            stream
            for stream in layout["streams"]
            if glob_match(stream["glob"], relative_path)
        ),
        None,
    )


def _stronger(first: str, second: str) -> str:
    return second if EVIDENCE_STRENGTH[second] > EVIDENCE_STRENGTH[first] else first


def _with_evidence(record_type: Mapping[str, Any], evidence: str) -> dict[str, Any]:
    value = copy.deepcopy(dict(record_type))
    value["evidence"] = value.get("evidence", evidence)
    return value


def _merge_record_types(
    target: dict[str, dict[str, Any]],
    incoming: Mapping[str, Mapping[str, Any]],
    doc_evidence: str,
) -> None:
    for discriminant, record_type in incoming.items():
        evidence = record_type.get("evidence", doc_evidence)
        current = target.get(discriminant)
        if current is None:
            target[discriminant] = _with_evidence(record_type, evidence)
            continue
        current["observed"] += record_type["observed"]
        current["shape"] = merge_shapes(current["shape"], record_type["shape"])
        current["evidence"] = _stronger(current.get("evidence", evidence), evidence)


def merge_docs(documents: Iterable[Mapping[str, Any]]) -> dict[str, Any]:
    docs = [copy.deepcopy(dict(document)) for document in documents]
    if not docs:
        raise GroundTruthError("no source documents to merge")
    agent = docs[0].get("agent")
    if any(document.get("agent") != agent for document in docs):
        agents = sorted({str(document.get("agent")) for document in docs})
        raise GroundTruthError(f"mixed agents in sources: {agents}")
    for document in docs:
        if document.get("format") != FORMAT:
            raise GroundTruthError(f"source for {agent}: expected format {FORMAT!r}")
        if document.get("evidence") not in EVIDENCE_STRENGTH:
            raise GroundTruthError(f"source for {agent}: invalid evidence")
    docs.sort(key=lambda document: EVIDENCE_STRENGTH[document["evidence"]], reverse=True)

    streams: list[dict[str, Any]] = []
    by_id: dict[str, dict[str, Any]] = {}
    for document in docs:
        evidence = document["evidence"]
        for source_stream in document.get("streams", []):
            stream_id = source_stream["id"]
            target = by_id.get(stream_id)
            if target is None:
                target = copy.deepcopy(source_stream)
                target["evidence"] = source_stream.get("evidence", evidence)
                target["record_types"] = {
                    key: _with_evidence(value, evidence)
                    for key, value in target.get("record_types", {}).items()
                }
                for facet in target.get("facets", []):
                    facet["record_types"] = {
                        key: _with_evidence(value, evidence)
                        for key, value in facet.get("record_types", {}).items()
                    }
                streams.append(target)
                by_id[stream_id] = target
                continue

            target["evidence"] = _stronger(
                target.get("evidence", evidence),
                source_stream.get("evidence", evidence),
            )
            if not target.get("discriminator") and source_stream.get("discriminator"):
                target["discriminator"] = source_stream["discriminator"]
            target.setdefault("record_types", {})
            _merge_record_types(
                target["record_types"], source_stream.get("record_types", {}), evidence
            )

            target_facets = {facet["id"]: facet for facet in target.setdefault("facets", [])}
            for source_facet in source_stream.get("facets", []):
                facet = target_facets.get(source_facet["id"])
                if facet is None:
                    facet = copy.deepcopy(source_facet)
                    facet["record_types"] = {
                        key: _with_evidence(value, evidence)
                        for key, value in facet.get("record_types", {}).items()
                    }
                    target["facets"].append(facet)
                    target_facets[facet["id"]] = facet
                else:
                    _merge_record_types(
                        facet.setdefault("record_types", {}),
                        source_facet.get("record_types", {}),
                        evidence,
                    )

    for stream in streams:
        if stream.get("record_types"):
            stream["record_types"] = dict(sorted(stream["record_types"].items()))
        else:
            stream.pop("record_types", None)
        for facet in stream.get("facets", []):
            facet["record_types"] = dict(sorted(facet.get("record_types", {}).items()))
        if not stream.get("facets"):
            stream.pop("facets", None)

    strongest = docs[0]
    return {
        "format": FORMAT,
        "agent": agent,
        "agent_version": strongest["agent_version"],
        "evidence": strongest["evidence"],
        "generator": "tools.groundtruth canonical 2",
        "data_root": strongest["data_root"],
        "sources": [
            {
                **{
                    "generator": document["generator"],
                    "agent_version": document["agent_version"],
                    "evidence": document["evidence"],
                },
                **(
                    {"path": document["_source_path"]}
                    if document.get("_source_path") is not None
                    else {}
                ),
                **(
                    {"sha256": document["_source_sha256"]}
                    if document.get("_source_sha256") is not None
                    else {}
                ),
                **(
                    {"captured_at": document["captured_at"]}
                    if document.get("captured_at") is not None
                    else {}
                ),
                **(
                    {"writer_version": document["writer_version"]}
                    if document.get("writer_version") is not None
                    else {}
                ),
            }
            for document in docs
        ],
        "streams": streams,
    }


def canonical_from_sources(agent_dir: Path) -> dict[str, Any]:
    source_dir = agent_dir / "sources"
    paths = sorted(source_dir.glob("*.json"))
    if not paths:
        raise GroundTruthError(f"{source_dir}: no JSON source documents")
    documents = []
    for path in paths:
        document = read_json(path)
        document["_source_path"] = f"sources/{path.name}"
        document["_source_sha256"] = hashlib.sha256(path.read_bytes()).hexdigest()
        documents.append(document)
    return merge_docs(documents)


def build_canonical(agent_dir: Path) -> Path:
    canonical = canonical_from_sources(agent_dir)
    output = agent_dir / "groundtruth.json"
    write_json(output, canonical)
    return output


def scrub_shape(shape: dict[str, Any], needles: Iterable[str]) -> None:
    lowered = tuple(needle.lower() for needle in needles if len(needle) >= 3)
    kind = shape.get("kind")
    if kind == "string" and "values" in shape:
        if any(
            needle in value.lower()
            for value in shape["values"]
            for needle in lowered
        ):
            shape.pop("values", None)
    elif kind == "array" and "items" in shape:
        scrub_shape(shape["items"], lowered)
    elif kind == "object":
        for field in shape.get("fields", {}).values():
            scrub_shape(field["shape"], lowered)
    elif kind == "any_of":
        for variant in shape.get("variants", []):
            scrub_shape(variant, lowered)
