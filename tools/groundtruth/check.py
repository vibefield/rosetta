"""Validate repository boundaries and committed ground-truth artifacts."""

from __future__ import annotations

import argparse
import sys
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
)
from .render import render, render_agent


def _agent_directories(root: Path) -> list[Path]:
    groundtruth = root / "groundtruth"
    return sorted(
        path
        for path in groundtruth.iterdir()
        if path.is_dir() and (path / "watch.toml").is_file()
    )


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


def validate(root: Path, *, fix: bool = False) -> list[str]:
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
        canonical_path = agent_dir / "groundtruth.json"
        if not canonical_path.exists():
            continue
        try:
            if fix:
                build_canonical(agent_dir)
                render_agent(agent_dir)
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
    args = parser.parse_args(argv)
    try:
        errors = validate(repo_root(), fix=args.fix)
    except GroundTruthError as error:
        errors = [str(error)]
    if errors:
        for error in errors:
            print(f"error: {error}", file=sys.stderr)
        return 1
    print("ground-truth structure and generated artifacts are consistent")
    return 0
