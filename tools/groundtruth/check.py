"""Validate repository boundaries and committed ground-truth artifacts."""

from __future__ import annotations

import argparse
import sys
import tomllib
from pathlib import Path
from typing import Any

from . import FORMAT
from .model import (
    GroundTruthError,
    build_canonical,
    canonical_from_sources,
    glob_match,
    load_layout,
    read_json,
    repo_root,
    sanitize_census_document,
    write_json,
)
from .render import render, render_agent


def _agent_directories(root: Path) -> list[Path]:
    groundtruth = root / "groundtruth"
    return sorted(
        path
        for path in groundtruth.iterdir()
        if path.is_dir() and (path / "watch.toml").is_file()
    )


def _load_watch(agent_dir: Path) -> dict[str, Any]:
    path = agent_dir / "watch.toml"
    try:
        value = tomllib.loads(path.read_text())
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise GroundTruthError(f"{path}: {error}") from error
    return value


def _fixture_inventory(scenario_dir: Path) -> list[dict[str, Any]]:
    return [
        {
            "path": path.relative_to(scenario_dir).as_posix(),
            "bytes": path.stat().st_size,
        }
        for path in sorted(scenario_dir.rglob("*"))
        if path.is_file() and path != scenario_dir / "manifest.json"
    ]


def _validate_manifest(
    manifest_path: Path,
    *,
    agent: str,
    version: str,
    scenario: str,
    dialect: str,
    fix: bool,
) -> list[str]:
    try:
        current = read_json(manifest_path)
    except GroundTruthError as error:
        return [str(error)]
    expected = {
        "agent": agent,
        "agent_version": version,
        "scenario": scenario,
        "generator": "tools.groundtruth farm 1",
        "mock_dialect": dialect,
        "files": _fixture_inventory(manifest_path.parent),
    }
    if current == expected:
        return []
    if fix:
        write_json(manifest_path, expected)
        return []

    current_files = current.get("files")
    listed: dict[str, Any] = {}
    duplicate = False
    unsafe = False
    if isinstance(current_files, list):
        for item in current_files:
            if not isinstance(item, dict) or not isinstance(item.get("path"), str):
                unsafe = True
                continue
            relative = Path(item["path"])
            if relative.is_absolute() or ".." in relative.parts:
                unsafe = True
            if item["path"] in listed:
                duplicate = True
            listed[item["path"]] = item.get("bytes")
    else:
        unsafe = True
    actual = {item["path"]: item["bytes"] for item in expected["files"]}
    missing = sorted(set(actual) - set(listed))
    extra = sorted(set(listed) - set(actual))
    resized = sorted(
        path for path in set(actual) & set(listed) if actual[path] != listed[path]
    )
    metadata = [
        key for key in expected if key != "files" and current.get(key) != expected[key]
    ]
    details = []
    if metadata:
        details.append("metadata=" + ",".join(metadata))
    if missing:
        details.append("missing=" + ",".join(missing[:6]))
    if extra:
        details.append("extra=" + ",".join(extra[:6]))
    if resized:
        details.append("resized=" + ",".join(resized[:6]))
    if duplicate:
        details.append("duplicate paths")
    if unsafe:
        details.append("malformed/unsafe entries")
    return [f"{manifest_path}: stale manifest ({'; '.join(details)})"]


def _stream_for(document: dict[str, Any], relative: str) -> dict[str, Any] | None:
    return next(
        (
            stream
            for stream in document.get("streams", [])
            if not stream["id"].startswith("farm:")
            and glob_match(stream["glob"], relative)
        ),
        None,
    )


def _is_evidence_payload(path: Path, groundtruth: Path) -> bool:
    if not path.is_relative_to(groundtruth):
        return False
    return any(
        part in {"fixtures", "sources", "census"}
        for part in path.relative_to(groundtruth).parts
    )


def _validate_census_sources(agent_dir: Path, *, fix: bool) -> list[str]:
    errors: list[str] = []
    for path in sorted((agent_dir / "sources").glob("*.json")):
        try:
            document = read_json(path)
        except GroundTruthError as error:
            errors.append(str(error))
            continue
        if not str(document.get("generator", "")).startswith(
            "tools.groundtruth census"
        ):
            continue
        sanitized = sanitize_census_document(document)
        if sanitized == document:
            continue
        if fix:
            write_json(path, sanitized)
        else:
            errors.append(
                f"{path}: organic census does not satisfy the enum/value privacy policy"
            )
    return errors


def _blocked_agents(root: Path) -> list[str]:
    blocked = []
    for agent_dir in _agent_directories(root):
        try:
            watch = _load_watch(agent_dir)
        except GroundTruthError:
            continue
        if watch.get("groundtruth", {}).get("status") == "blocked":
            blocked.append(agent_dir.name)
    return blocked


def validate(
    root: Path, *, fix: bool = False, require_complete: bool = False
) -> list[str]:
    errors: list[str] = []
    groundtruth = root / "groundtruth"
    if not groundtruth.is_dir():
        return ["missing groundtruth/ directory"]

    root_entries = [path for path in groundtruth.iterdir() if not path.is_dir()]
    errors.extend(
        f"groundtruth/ must contain only agent directories: {path.name}"
        for path in root_entries
    )
    for directory in (path for path in groundtruth.iterdir() if path.is_dir()):
        if not (directory / "watch.toml").is_file():
            errors.append(
                f"groundtruth/ child is not an agent data directory: {directory.name}/"
            )
    for forbidden in ("runtime", "services", "rosettad"):
        if (root / forbidden).exists():
            errors.append(f"legacy top-level path still exists: {forbidden}/")

    for path in root.rglob("*.rs"):
        if not path.is_relative_to(root / "crates") and not _is_evidence_payload(
            path, groundtruth
        ):
            errors.append(f"Rust source must live under crates/: {path.relative_to(root)}")
    for manifest in root.rglob("Cargo.toml"):
        if manifest == root / "Cargo.toml":
            continue
        if not manifest.is_relative_to(root / "crates") and not _is_evidence_payload(
            manifest, groundtruth
        ):
            errors.append(f"Rust crate must live under crates/: {manifest.relative_to(root)}")

    agents = _agent_directories(root)
    if not agents:
        errors.append("groundtruth/ contains no agent directories")
    for agent_dir in agents:
        code_at_data_root = [
            path
            for path in agent_dir.iterdir()
            if path.is_file() and path.suffix in {".rs", ".py", ".js", ".ts"}
        ]
        if (
            code_at_data_root
            or (agent_dir / "src").exists()
            or (agent_dir / "Cargo.toml").exists()
        ):
            errors.append(f"{agent_dir.name}: ground-truth directories must be data-only")

        try:
            watch = _load_watch(agent_dir)
        except GroundTruthError as error:
            errors.append(str(error))
            continue
        status = watch.get("groundtruth", {}).get("status")
        if status not in {"captured", "blocked"}:
            errors.append(
                f"{agent_dir.name}: watch.toml must declare groundtruth.status as "
                "captured or blocked"
            )
            continue
        known_gaps = watch.get("groundtruth", {}).get("known_gaps", [])
        if not isinstance(known_gaps, list) or any(
            not isinstance(gap, str) or not gap.strip() for gap in known_gaps
        ):
            errors.append(
                f"{agent_dir.name}: groundtruth.known_gaps must be non-empty strings"
            )
        if status == "blocked":
            if not str(watch.get("groundtruth", {}).get("reason", "")).strip():
                errors.append(f"{agent_dir.name}: blocked ground truth needs a reason")
            if require_complete:
                errors.append(f"{agent_dir.name}: ground truth is explicitly blocked")
            continue

        pinned = str(watch.get("release", {}).get("pinned", "")).strip()
        if not pinned:
            errors.append(f"{agent_dir.name}: captured ground truth needs release.pinned")
            continue
        required = [
            agent_dir / "layout.toml",
            agent_dir / "sources",
            agent_dir / "fixtures" / pinned,
        ]
        missing = [path.name for path in required if not path.exists()]
        if missing:
            errors.append(
                f"{agent_dir.name}: captured pack is missing " + ", ".join(missing)
            )
            continue

        errors.extend(_validate_census_sources(agent_dir, fix=fix))
        dialect = str(watch.get("fixture", {}).get("mock_dialect", ""))
        fixture_version = agent_dir / "fixtures" / pinned
        scenario_dirs = sorted(path for path in fixture_version.iterdir() if path.is_dir())
        if not scenario_dirs:
            errors.append(f"{agent_dir.name}: pinned fixture corpus has no scenarios")
        for scenario_dir in scenario_dirs:
            manifest_path = scenario_dir / "manifest.json"
            if not manifest_path.is_file():
                errors.append(f"{manifest_path}: missing fixture manifest")
                continue
            errors.extend(
                _validate_manifest(
                    manifest_path,
                    agent=agent_dir.name,
                    version=pinned,
                    scenario=scenario_dir.name,
                    dialect=dialect,
                    fix=fix,
                )
            )

        canonical_path = agent_dir / "groundtruth.json"
        try:
            if fix:
                build_canonical(agent_dir)
                render_agent(agent_dir)
            if not canonical_path.is_file():
                errors.append(f"{agent_dir.name}: missing canonical groundtruth.json")
                continue
            document = read_json(canonical_path)
            expected = canonical_from_sources(agent_dir)
            layout = load_layout(agent_dir)
        except GroundTruthError as error:
            errors.append(str(error))
            continue
        if document != expected:
            errors.append(
                f"{agent_dir.name}: groundtruth.json is stale; run "
                f"python3 -m tools.groundtruth farm {agent_dir.name} --schemas-only"
            )
        if document.get("format") != FORMAT:
            errors.append(f"{canonical_path}: expected format {FORMAT}")
        if document.get("agent") != agent_dir.name:
            errors.append(f"{canonical_path}: agent does not match directory name")
        if document.get("agent_version") != pinned:
            errors.append(
                f"{canonical_path}: canonical version {document.get('agent_version')!r} "
                f"does not match pinned {pinned!r}"
            )
        stream_ids = [stream.get("id") for stream in document.get("streams", [])]
        if len(stream_ids) != len(set(stream_ids)):
            errors.append(f"{canonical_path}: duplicate stream ids")

        canonical_streams = {stream["id"]: stream for stream in document.get("streams", [])}
        for spec in layout["streams"]:
            stream = canonical_streams.get(spec["id"])
            if stream is None:
                errors.append(f"{agent_dir.name}: canonical document lacks layout stream {spec['id']}")
                continue
            for key in ("glob", "file_format", "discipline"):
                if stream.get(key) != spec.get(key):
                    errors.append(f"{agent_dir.name}/{spec['id']}: canonical {key} differs from layout")

        report_path = agent_dir / "groundtruth.html"
        expected_report = render([document])
        if not report_path.is_file():
            errors.append(f"{agent_dir.name}: missing generated groundtruth.html")
        elif report_path.read_text() != expected_report:
            errors.append(
                f"{agent_dir.name}: groundtruth.html is stale; run "
                f"python3 -m tools.groundtruth render {canonical_path} -o {report_path}"
            )

        fixtures = agent_dir / "fixtures"
        if fixtures.is_dir():
            unmatched: set[str] = set()
            covered: set[str] = set()
            for path in fixtures.rglob("*"):
                if not path.is_file():
                    continue
                fixture_parts = path.relative_to(fixtures).parts
                # <version>/<scenario>/<harvested-agent-root>/<path inside root>
                if len(fixture_parts) < 4:
                    continue
                relative = Path(*fixture_parts[3:]).as_posix()
                stream = _stream_for(document, relative)
                if stream is None:
                    unmatched.add(relative)
                else:
                    covered.add(stream["id"])
            if unmatched:
                errors.append(
                    f"{agent_dir.name}: fixture files outside the canonical layout: "
                    + ", ".join(sorted(unmatched)[:10])
                )
            for stream in document.get("streams", []):
                if (
                    stream.get("evidence") == "fixture"
                    and not stream["id"].startswith("farm:")
                    and stream["id"] not in covered
                ):
                    errors.append(
                        f"{agent_dir.name}: fixture-backed stream {stream['id']} has no fixture file"
                    )
    return errors


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fix", action="store_true", help="rebuild canonical JSON and HTML first")
    parser.add_argument(
        "--require-complete",
        action="store_true",
        help="fail when any roster agent is explicitly blocked",
    )
    args = parser.parse_args(argv)
    try:
        root = repo_root()
        errors = validate(
            root, fix=args.fix, require_complete=args.require_complete
        )
    except GroundTruthError as error:
        errors = [str(error)]
    if errors:
        for error in errors:
            print(f"error: {error}", file=sys.stderr)
        return 1
    blocked = _blocked_agents(root)
    suffix = f"; blocked: {', '.join(blocked)}" if blocked else ""
    print(f"ground-truth structure and generated artifacts are consistent{suffix}")
    return 0
