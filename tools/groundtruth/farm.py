"""Generate hermetic agent fixtures and rebuild canonical ground truth."""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import sqlite3
import subprocess
import tempfile
import tomllib
from collections import defaultdict
from contextlib import contextmanager
from pathlib import Path
from typing import Any, Iterator

from . import FORMAT
from .facets import extract as extract_facets
from .mock_model import start_server
from .model import (
    GroundTruthError,
    ShapeAccumulator,
    build_canonical,
    load_layout,
    matching_stream,
    repo_root,
    write_json,
)
from .render import render_agent

RUN_TIMEOUT_SECONDS = 240
GENERATOR = "tools.groundtruth farm 1"

SCENARIOS: dict[str, tuple[dict[str, Any], ...]] = {
    "claude-code": (
        {"name": "basic-chat", "prompts": ("Reply with a short greeting.",), "extra_args": ()},
        {
            "name": "tool-use",
            "prompts": ("Run the echo command now. [use-tool]",),
            "extra_args": ("--permission-mode", "bypassPermissions"),
        },
        {
            "name": "resume",
            "prompts": ("Reply with a short greeting.", "Continue: say goodbye."),
            "extra_args": (),
        },
    ),
    "codex": (
        {"name": "basic-chat", "prompts": ("Reply with a short greeting.",), "extra_args": ()},
        {
            "name": "tool-use",
            "prompts": ("Run the echo command now. [use-tool]",),
            "extra_args": ("--dangerously-bypass-approvals-and-sandbox",),
        },
        {
            "name": "resume",
            "prompts": ("Reply with a short greeting.", "Continue: say goodbye."),
            "extra_args": (),
        },
    ),
}


def _load_watch(agent_dir: Path) -> dict[str, Any]:
    path = agent_dir / "watch.toml"
    try:
        return tomllib.loads(path.read_text())
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise GroundTruthError(f"{path}: {error}") from error


def _selected_version(
    watch: dict[str, Any], override: str | None, *, schemas_only: bool
) -> str:
    if override:
        return override
    key = "pinned" if schemas_only else "surveyed"
    version = watch.get("release", {}).get(key)
    if not isinstance(version, str) or not version:
        raise GroundTruthError(f"no --version and no release.{key} in watch.toml")
    return version


@contextmanager
def _farm_lock() -> Iterator[None]:
    path = Path("/tmp/rosetta-farm.lock")
    try:
        descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    except FileExistsError as error:
        raise GroundTruthError(
            f"another farm run appears active ({path} exists; remove it if stale)"
        ) from error
    try:
        os.write(descriptor, f"{os.getpid()}\n".encode())
        os.close(descriptor)
        yield
    finally:
        path.unlink(missing_ok=True)


def _run(command: list[str], *, cwd: Path | None = None, env: dict[str, str] | None = None) -> None:
    try:
        subprocess.run(command, cwd=cwd, env=env, check=True)
    except (OSError, subprocess.CalledProcessError) as error:
        raise GroundTruthError(f"command failed: {' '.join(command)}: {error}") from error


def _npm_install(prefix: Path, package: str, version: str) -> None:
    prefix.mkdir(parents=True, exist_ok=True)
    print(f"installing {package}@{version} …")
    _run(
        [
            "npm",
            "install",
            "--prefix",
            str(prefix),
            "--no-fund",
            "--no-audit",
            "--loglevel=error",
            f"{package}@{version}",
        ]
    )


def _detect_version(binary: Path) -> str:
    if not binary.exists():
        raise GroundTruthError(f"binary not found at {binary}")
    try:
        output = subprocess.run(
            [str(binary), "--version"], check=True, capture_output=True, text=True
        ).stdout
    except (OSError, subprocess.CalledProcessError) as error:
        raise GroundTruthError(f"could not run {binary} --version: {error}") from error
    match = re.search(r"\b\d+(?:\.\d+)+[^\s]*", output)
    if match is None:
        raise GroundTruthError(f"could not detect installed version from {output!r}")
    return match.group(0)


def _scenario_step(
    command: list[str],
    scenario_dir: Path,
    scenario: str,
    step: int,
    env: dict[str, str],
    project: Path,
) -> None:
    stdout_name = "stdout.jsonl" if step == 0 else f"stdout-{step}.jsonl"
    stderr_name = "stderr.log" if step == 0 else f"stderr-{step}.log"
    try:
        result = subprocess.run(
            command,
            cwd=project,
            env=env,
            capture_output=True,
            timeout=RUN_TIMEOUT_SECONDS,
        )
    except subprocess.TimeoutExpired as error:
        raise GroundTruthError(
            f"scenario {scenario} step {step} timed out after {RUN_TIMEOUT_SECONDS}s"
        ) from error
    scenario_dir.mkdir(parents=True, exist_ok=True)
    (scenario_dir / stdout_name).write_bytes(result.stdout)
    (scenario_dir / stderr_name).write_bytes(result.stderr)
    if result.returncode:
        tail = result.stderr.decode(errors="replace").splitlines()[-15:]
        raise GroundTruthError(
            f"scenario {scenario} step {step} exited {result.returncode}; stderr tail:\n"
            + "\n".join(tail)
        )


def _harvest(tree: Path, tree_name: str, scenario_dir: Path, destination: Path) -> None:
    if destination.exists():
        shutil.rmtree(destination)
    destination.mkdir(parents=True)
    shutil.copytree(tree, destination / tree_name)
    for path in scenario_dir.iterdir():
        if path.is_file():
            shutil.copy2(path, destination / path.name)


def _redact(destination: Path, scenario_dir: Path, work: Path) -> None:
    replacements = [
        (str(scenario_dir), "<SCENARIO_DIR>"),
        (f"/private{scenario_dir}", "<SCENARIO_DIR>"),
        (str(work), "<WORK>"),
        (f"/private{work}", "<WORK>"),
    ]
    username = os.environ.get("USER", "")
    if username:
        replacements.extend(
            [
                (f"/Users/{username}", "/Users/<USER>"),
                (f"/home/{username}", "/home/<USER>"),
            ]
        )
    replacements.sort(key=lambda pair: len(pair[0]), reverse=True)
    for path in destination.rglob("*"):
        if not path.is_file():
            continue
        try:
            text = path.read_text()
        except (OSError, UnicodeDecodeError):
            continue
        for needle, replacement in replacements:
            text = text.replace(needle, replacement)
        path.write_text(text)


def _manifest(
    destination: Path,
    agent: str,
    version: str,
    scenario: str,
    dialect: str,
) -> None:
    files = [
        {"path": path.relative_to(destination).as_posix(), "bytes": path.stat().st_size}
        for path in sorted(destination.rglob("*"))
        if path.is_file() and path.name != "manifest.json"
    ]
    write_json(
        destination / "manifest.json",
        {
            "agent": agent,
            "agent_version": version,
            "scenario": scenario,
            "generator": GENERATOR,
            "mock_dialect": dialect,
            "files": files,
        },
    )


def _clean_project(scenario: str) -> Path:
    scenario_root = Path("/tmp/rosetta-farm") / scenario
    if scenario_root.exists():
        shutil.rmtree(scenario_root)
    project = scenario_root / "project"
    project.mkdir(parents=True)
    return project


def _minimal_env(**values: str | Path) -> dict[str, str]:
    environment = {"PATH": os.environ.get("PATH", ""), "NO_COLOR": "1", "TERM": "dumb"}
    environment.update({key: str(value) for key, value in values.items()})
    return environment


def _generate_fixtures(agent: str, agent_dir: Path, requested: str, keep_work: bool) -> str:
    watch = _load_watch(agent_dir)
    work = Path(tempfile.gettempdir()) / f"rosetta-farm-{agent}-{os.getpid()}"
    if work.exists():
        shutil.rmtree(work)
    work.mkdir(parents=True)
    print(f"work dir: {work}")

    with _farm_lock():
        install = work / "install"
        package = watch["release"]["id"]
        if agent == "codex" and "/" in package and not package.startswith("@"):
            package = "@openai/codex"
        _npm_install(install, package, requested)

        if agent == "claude-code":
            _run(
                [
                    "node",
                    str(install / "node_modules/@anthropic-ai/claude-code/install.cjs"),
                ]
            )
            binary = install / "node_modules/.bin/claude"
        else:
            binary = install / "node_modules/.bin/codex"
        actual = _detect_version(binary)
        print(f"installed version: {actual}")

        server, thread = start_server()
        host, port = server.server_address
        base_url = f"http://{host}:{port}"
        print(f"mockmodel at {base_url}")
        try:
            fixture_root = agent_dir / "fixtures" / actual
            for scenario in SCENARIOS[agent]:
                if not thread.is_alive():
                    raise GroundTruthError(
                        f"mockmodel died before scenario {scenario['name']}"
                    )
                name = scenario["name"]
                print(f"scenario: {name}")
                scenario_dir = work / "scenarios" / name
                home = scenario_dir / "home"
                agent_home = scenario_dir / (
                    "claude-config" if agent == "claude-code" else "codex-home"
                )
                project = _clean_project(name)
                home.mkdir(parents=True)
                agent_home.mkdir(parents=True)

                if agent == "codex":
                    (agent_home / "config.toml").write_text(
                        'model = "mock-model"\nmodel_provider = "mock"\n\n'
                        "[model_providers.mock]\n"
                        'name = "mock"\n'
                        f'base_url = "{base_url}/v1"\n'
                        'env_key = "MOCK_API_KEY"\n'
                        'wire_api = "responses"\n'
                    )

                for step, prompt in enumerate(scenario["prompts"]):
                    if agent == "claude-code":
                        command = [
                            str(binary),
                            "-p",
                            prompt,
                            "--output-format",
                            "stream-json",
                            "--verbose",
                            "--bare",
                            *scenario["extra_args"],
                        ]
                        if step:
                            command.append("-c")
                        environment = _minimal_env(
                            HOME=home,
                            CLAUDE_CONFIG_DIR=agent_home,
                            ANTHROPIC_BASE_URL=base_url,
                            ANTHROPIC_API_KEY="rosetta-mock-key",
                            CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC="1",
                        )
                    else:
                        command = [str(binary), "exec"]
                        if step:
                            command.extend(["resume", "--last"])
                        command.extend(
                            [
                                prompt,
                                "--json",
                                "--skip-git-repo-check",
                                *scenario["extra_args"],
                            ]
                        )
                        environment = _minimal_env(
                            HOME=home,
                            CODEX_HOME=agent_home,
                            MOCK_API_KEY="rosetta-mock-key",
                        )
                    _scenario_step(
                        command, scenario_dir, name, step, environment, project
                    )

                tree_name = "claude-config" if agent == "claude-code" else "codex-home"
                destination = fixture_root / name
                _harvest(agent_home, tree_name, scenario_dir, destination)
                if agent == "claude-code":
                    shutil.rmtree(destination / tree_name / "backups", ignore_errors=True)
                    dialect = "anthropic-messages"
                else:
                    (destination / tree_name / "auth.json").unlink(missing_ok=True)
                    shutil.rmtree(destination / tree_name / ".tmp", ignore_errors=True)
                    shutil.rmtree(destination / tree_name / "skills", ignore_errors=True)
                    dialect = "openai-responses"
                _redact(destination, scenario_dir, work)
                _manifest(destination, agent, actual, name, dialect)
                print(f"harvested -> {destination}")
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=5)

    if keep_work:
        print(f"work dir kept: {work}")
    else:
        shutil.rmtree(work, ignore_errors=True)
    return actual


def _quote_identifier(value: str) -> str:
    return '"' + value.replace('"', '""') + '"'


def _sqlite_shape(declared_type: str) -> dict[str, Any]:
    declared = declared_type.upper()
    if any(token in declared for token in ("INT", "REAL", "FLOA", "DOUB")):
        return {"kind": "number"}
    if "BOOL" in declared:
        return {"kind": "boolean"}
    if any(token in declared for token in ("CHAR", "CLOB", "TEXT")):
        return {"kind": "string"}
    return {"kind": "unknown"}


def _absorb_sqlite(
    accumulator: ShapeAccumulator,
    database_name: str,
    path: Path,
    schemas: dict[str, list[str]],
) -> None:
    uri = f"{path.resolve().as_uri()}?mode=ro"
    try:
        connection = sqlite3.connect(uri, uri=True)
        if database_name not in schemas:
            schemas[database_name] = [
                f"{row[0]};"
                for row in connection.execute(
                    "SELECT sql FROM sqlite_master "
                    "WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite_%' ORDER BY name"
                )
            ]
        tables = [
            row[0]
            for row in connection.execute(
                "SELECT name FROM sqlite_master "
                "WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name"
            )
        ]
        for table in tables:
            columns = connection.execute(
                f"PRAGMA table_info({_quote_identifier(table)})"
            ).fetchall()
            rows = connection.execute(
                f"SELECT count(*) FROM {_quote_identifier(table)}"
            ).fetchone()[0]
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
    except sqlite3.Error as error:
        raise GroundTruthError(f"introspecting {path}: {error}") from error
    finally:
        if "connection" in locals():
            connection.close()


def _record_type(value: Any) -> str:
    return value.get("type", "<untyped>") if isinstance(value, dict) else "<untyped>"


def emit_groundtruth(agent_dir: Path, version: str) -> tuple[Path, Path]:
    layout = load_layout(agent_dir)
    fixture_root = agent_dir / "fixtures" / version
    if not fixture_root.is_dir():
        raise GroundTruthError(f"no fixtures at {fixture_root}; run the farm first")
    accumulators: dict[str, ShapeAccumulator] = defaultdict(ShapeAccumulator)
    facet_accumulators: dict[tuple[str, str], tuple[str, str, ShapeAccumulator]] = {}
    sqlite_schemas: dict[str, list[str]] = {}

    for path in sorted(fixture_root.rglob("*")):
        if not path.is_file():
            continue
        fixture_parts = path.relative_to(fixture_root).parts
        # <scenario>/<harvested-agent-root>/<path inside that root>
        if len(fixture_parts) >= 3:
            relative = Path(*fixture_parts[2:]).as_posix()
            stream = matching_stream(layout, relative)
            if stream is None:
                continue
            stream_id = stream["id"]
            is_sqlite = stream["file_format"] == "sqlite"
        elif path.name.startswith("stdout") and path.suffix == ".jsonl":
            stream_id = "farm:headless-stdout"
            is_sqlite = False
        else:
            continue

        if is_sqlite:
            if path.suffix == ".sqlite":
                _absorb_sqlite(
                    accumulators[stream_id], path.name, path, sqlite_schemas
                )
            continue
        if path.suffix not in {".json", ".jsonl"}:
            continue
        try:
            text = path.read_text()
        except (OSError, UnicodeDecodeError) as error:
            raise GroundTruthError(f"{path}: {error}") from error
        if path.suffix == ".jsonl":
            context: dict[str, str] = {}
            for line in text.splitlines():
                if not line.strip():
                    continue
                try:
                    value = json.loads(line)
                except json.JSONDecodeError:
                    continue
                accumulators[stream_id].absorb(_record_type(value), value)
                if isinstance(value, dict):
                    for hit in extract_facets(
                        layout["agent"], stream_id, value, context
                    ):
                        key = (stream_id, hit["facet_id"])
                        if key not in facet_accumulators:
                            facet_accumulators[key] = (
                                hit["path"],
                                hit["discriminator"],
                                ShapeAccumulator(),
                            )
                        facet_accumulators[key][2].absorb(hit["key"], hit["value"])
        else:
            try:
                value = json.loads(text)
            except json.JSONDecodeError:
                continue
            accumulators[stream_id].absorb("", value)

    source_dir = agent_dir / "sources"
    source_dir.mkdir(parents=True, exist_ok=True)
    if sqlite_schemas:
        sections = [
            f"-- SQLite schema dump for {layout['agent']} {version} — generated by {GENERATOR}.\n"
            "-- One section per database file; diff between versions IS the drift report.\n"
        ]
        for database, statements in sorted(sqlite_schemas.items()):
            sections.append(f"\n-- ===== {database} =====\n")
            sections.extend(f"{statement}\n" for statement in statements)
        schema_path = source_dir / f"{version}.sqlite-schema.sql"
        schema_path.write_text("".join(sections))
        print(f"sqlite schema dump -> {schema_path}")

    facets_by_stream: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for (stream_id, facet_id), (path, discriminator, accumulator) in sorted(
        facet_accumulators.items()
    ):
        facets_by_stream[stream_id].append(
            {
                "id": facet_id,
                "path": path,
                "discriminator": discriminator,
                "record_types": accumulator.into_types(),
            }
        )

    streams: list[dict[str, Any]] = []
    for spec in layout["streams"]:
        record_types = (
            accumulators.pop(spec["id"]).into_types()
            if spec["id"] in accumulators
            else {}
        )
        stream = {
            "id": spec["id"],
            "glob": spec["glob"],
            "file_format": spec["file_format"],
            "discipline": spec["discipline"],
            "evidence": spec["evidence"],
            "notes": spec.get("notes", ""),
        }
        if spec["file_format"] == "jsonl" and any(record_types):
            stream["discriminator"] = "type"
        elif spec["file_format"] == "sqlite" and record_types:
            stream["discriminator"] = "table"
        if record_types:
            stream["record_types"] = record_types
        if facets_by_stream.get(spec["id"]):
            stream["facets"] = facets_by_stream[spec["id"]]
        streams.append(stream)

    stdout = accumulators.pop("farm:headless-stdout", None)
    if stdout is not None:
        streams.append(
            {
                "id": "farm:headless-stdout",
                "glob": "<farm>/stdout*.jsonl",
                "file_format": "jsonl",
                "discipline": "append_only",
                "evidence": "fixture",
                "notes": "Headless stream-json stdout — corroboration surface captured by the farm, not part of the agent data root.",
                "discriminator": "type",
                "record_types": stdout.into_types(),
            }
        )

    intermediate = {
        "format": FORMAT,
        "agent": layout["agent"],
        "agent_version": version,
        "evidence": "fixture",
        "generator": GENERATOR,
        "data_root": layout["data_root"],
        "streams": streams,
    }
    source_path = source_dir / f"{version}.fixtures.json"
    write_json(source_path, intermediate)
    print(f"fixtures intermediate -> {source_path}")
    canonical = build_canonical(agent_dir)
    report = render_agent(agent_dir)
    print(f"canonical ground truth -> {canonical}")
    print(f"schema report -> {report}")
    return canonical, report


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("agent", choices=sorted(SCENARIOS))
    parser.add_argument("--version")
    parser.add_argument("--keep-work", action="store_true")
    parser.add_argument("--schemas-only", action="store_true")
    args = parser.parse_args(argv)
    try:
        root = repo_root()
        agent_dir = root / "groundtruth" / args.agent
        watch = _load_watch(agent_dir)
        requested = _selected_version(
            watch, args.version, schemas_only=args.schemas_only
        )
        version = (
            requested
            if args.schemas_only
            else _generate_fixtures(
                args.agent, agent_dir, requested, args.keep_work
            )
        )
        emit_groundtruth(agent_dir, version)
    except GroundTruthError as error:
        parser.error(str(error))
    print("done. Review the fixture, canonical JSON, and generated HTML diffs.")
    return 0
