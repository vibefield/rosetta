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
from .json_schema import write_copilot_writer_schema
from .mock_model import start_server
from .model import (
    GroundTruthError,
    ShapeAccumulator,
    build_canonical,
    jsonl_record_type,
    load_layout,
    matching_stream,
    merge_shapes,
    repo_root,
    shape_of,
    sqlite_schema_name,
    write_json,
)
from .render import render_agent

RUN_TIMEOUT_SECONDS = 240
GENERATOR = "tools.groundtruth farm 1"
ISOLATED_RUNNER_ENV = "ROSETTA_FARM_ISOLATED"

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
    "kimi-code": (
        {"name": "basic-chat", "prompts": ("Reply with a short greeting.",), "extra_args": ()},
        {
            "name": "tool-use",
            "prompts": ("Run the echo command now. [use-tool]",),
            "extra_args": (),
        },
        {
            "name": "resume",
            "prompts": ("Reply with a short greeting.", "Continue: say goodbye."),
            "extra_args": (),
        },
        {
            "name": "subagent",
            "prompts": ("Delegate a short greeting to a subagent. [use-agent]",),
            "extra_args": (),
        },
        {
            "name": "error",
            "prompts": ("This request must fail. [error]",),
            "extra_args": (),
            "allow_failure": True,
        },
    ),
    "grok": (
        {"name": "basic-chat", "prompts": ("Reply with a short greeting.",), "extra_args": ()},
        {
            "name": "tool-use",
            "prompts": ("Run the echo command now. [use-tool]",),
            "extra_args": (),
        },
        {
            "name": "resume",
            "prompts": ("Reply with a short greeting.", "Continue: say goodbye."),
            "extra_args": (),
        },
    ),
    "copilot": (
        {"name": "basic-chat", "prompts": ("Reply with a short greeting.",), "extra_args": ("--allow-all",)},
        {
            "name": "tool-use",
            "prompts": ("Run the echo command now. [use-tool]",),
            "extra_args": ("--allow-all",),
        },
        {
            "name": "resume",
            "prompts": ("Reply with a short greeting.", "Continue: say goodbye."),
            "extra_args": ("--allow-all",),
        },
        {
            "name": "tool-denied",
            "prompts": ("Run the echo command now. [use-tool]",),
            "extra_args": ("--allow-all", "--deny-tool=shell"),
        },
    ),
    "opencode": (
        {"name": "basic-chat", "prompts": ("Reply with a short greeting.",), "extra_args": ()},
        {
            "name": "tool-use",
            "prompts": ("Run the echo command now. [use-tool]",),
            "extra_args": (),
        },
        {
            "name": "resume",
            "prompts": ("Reply with a short greeting.", "Continue: say goodbye."),
            "extra_args": (),
        },
    ),
    "pi": (
        {
            "name": "basic-chat",
            "prompts": ("Reply with a short greeting.",),
            "extra_args": ("--name", "rosetta-basic-chat"),
        },
        {
            "name": "tool-use",
            "prompts": ("Run the echo command now. [use-tool]",),
            "extra_args": ("--tools", "bash"),
        },
        {
            "name": "resume",
            "prompts": ("Reply with a short greeting.", "Continue: say goodbye."),
            "extra_args": (),
        },
        {
            "name": "fork",
            "prompts": ("Reply with a short greeting.", "Continue from the fork."),
            "extra_args": (),
        },
        {
            "name": "error",
            "prompts": ("This request must fail. [error]",),
            "extra_args": (),
            "allow_failure": True,
        },
        {
            "name": "compaction",
            "prompts": (
                "Start a short conversation with a greeting.",
                "Add one more short greeting before compaction.",
                "[high-usage] Finish this turn so prior turns compact.",
            ),
            "extra_args": (),
            "expected_record": ("session", "compaction"),
        },
    ),
    "antigravity": (
        {"name": "basic-chat", "prompts": ("Reply with a short greeting.",), "extra_args": ()},
        {
            "name": "tool-use",
            "prompts": ("Run the echo command now. [use-tool]",),
            "extra_args": (),
        },
        {
            "name": "resume",
            "prompts": ("Reply with a short greeting.", "Continue: say goodbye."),
            "extra_args": (),
        },
    ),
    "hermes": (
        {"name": "basic-chat", "prompts": ("Reply with a short greeting.",), "extra_args": ()},
        {
            "name": "tool-use",
            "prompts": ("Run the echo command now. [use-tool]",),
            "extra_args": (),
        },
        {
            "name": "resume",
            "prompts": ("Reply with a short greeting.", "Continue: say goodbye."),
            "extra_args": (),
        },
    ),
}

TREE_NAMES = {
    "claude-code": "claude-config",
    "codex": "codex-home",
    "kimi-code": "kimi-home",
    "grok": "grok-home",
    "copilot": "copilot-home",
    "opencode": "opencode-home",
    "pi": "pi-home",
    "antigravity": "antigravity-cli",
    "hermes": "hermes-home",
}

BINARIES = {
    "claude-code": "claude",
    "codex": "codex",
    "kimi-code": "kimi",
    "copilot": "copilot",
    "opencode": "opencode",
    "pi": "pi",
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


def _running_in_container() -> bool:
    """Return whether the farm is running inside a common Linux container runtime."""

    return Path("/.dockerenv").exists() or Path("/run/.containerenv").exists()


def _require_isolated_runner(*, allow_host_execution: bool) -> None:
    """Refuse to install and execute downloaded agents on an ordinary host.

    ``ROSETTA_FARM_ISOLATED=1`` is an administrator/user attestation for isolated
    runners whose container/VM runtime does not expose one of the conventional
    marker files. ``--allow-host-execution`` is deliberately a separate, noisy
    escape hatch for controlled research machines.
    """

    if _running_in_container() or os.environ.get(ISOLATED_RUNNER_ENV) == "1":
        return
    if allow_host_execution:
        print(
            "WARNING: executing freshly downloaded agent code on the host because "
            "--allow-host-execution was supplied",
            file=os.sys.stderr,
        )
        return
    raise GroundTruthError(
        "fixture generation installs and executes downloaded agent code; run the "
        "farm in a container/VM (prefer a runner image pinned by sha256 digest), "
        f"set {ISOLATED_RUNNER_ENV}=1 for an independently isolated runner, or pass "
        "--allow-host-execution only on a disposable research machine"
    )


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


def _npm_install(
    prefix: Path, package: str, version: str, *, ignore_scripts: bool = False
) -> None:
    prefix.mkdir(parents=True, exist_ok=True)
    print(f"installing {package}@{version} …")
    command = [
        "npm",
        "install",
        "--prefix",
        str(prefix),
        "--no-fund",
        "--no-audit",
        "--loglevel=error",
    ]
    if ignore_scripts:
        command.append("--ignore-scripts")
    command.append(f"{package}@{version}")
    _run(command)


def _grok_platform() -> str:
    system = os.uname().sysname
    machine = os.uname().machine
    if system == "Darwin":
        os_name = "macos"
    elif system == "Linux":
        os_name = "linux"
    else:
        raise GroundTruthError(f"unsupported grok farm OS: {system}")
    if machine in {"x86_64", "amd64"}:
        arch = "x86_64"
        if os_name == "macos":
            try:
                probe = subprocess.run(
                    ["/usr/sbin/sysctl", "-n", "hw.optional.arm64"],
                    check=False,
                    capture_output=True,
                    text=True,
                )
            except OSError:
                probe = None
            if probe is not None and probe.stdout.strip() == "1":
                arch = "aarch64"
    elif machine in {"arm64", "aarch64"}:
        arch = "aarch64"
    else:
        raise GroundTruthError(f"unsupported grok farm architecture: {machine}")
    return f"{os_name}-{arch}"


def _install_grok(prefix: Path, version: str) -> Path:
    prefix.mkdir(parents=True, exist_ok=True)
    platform = _grok_platform()
    url = f"https://x.ai/cli/grok-{version}-{platform}"
    binary = prefix / "grok"
    print(f"downloading {url} …")
    try:
        subprocess.run(
            ["curl", "-fL", "--retry", "3", "-o", str(binary), url],
            check=True,
        )
    except (OSError, subprocess.CalledProcessError) as error:
        raise GroundTruthError(f"download failed: {url}: {error}") from error
    binary.chmod(0o755)
    agent = prefix / "agent"
    agent.unlink(missing_ok=True)
    agent.symlink_to(binary.name)
    return binary


def _agy_release_asset() -> str:
    system = os.uname().sysname
    machine = os.uname().machine
    if system == "Darwin" and machine in {"arm64", "aarch64"}:
        return "agy_cli_mac_arm64.tar.gz"
    if system == "Darwin":
        return "agy_cli_mac_x64.tar.gz"
    if system == "Linux" and machine in {"arm64", "aarch64"}:
        return "agy_cli_linux_arm64.tar.gz"
    if system == "Linux":
        return "agy_cli_linux_x64.tar.gz"
    raise GroundTruthError(f"unsupported antigravity farm platform: {system}/{machine}")


def _install_antigravity(prefix: Path, version: str) -> Path:
    prefix.mkdir(parents=True, exist_ok=True)
    asset = _agy_release_asset()
    url = (
        "https://github.com/google-antigravity/antigravity-cli/releases/download/"
        f"{version}/{asset}"
    )
    archive = prefix / asset
    print(f"downloading {url} …")
    try:
        subprocess.run(
            ["curl", "-fL", "--retry", "3", "-o", str(archive), url],
            check=True,
        )
    except (OSError, subprocess.CalledProcessError) as error:
        raise GroundTruthError(f"download failed: {url}: {error}") from error
    extract = prefix / "extract"
    extract.mkdir(parents=True, exist_ok=True)
    try:
        subprocess.run(["tar", "-xzf", str(archive), "-C", str(extract)], check=True)
    except (OSError, subprocess.CalledProcessError) as error:
        raise GroundTruthError(f"extract failed: {archive}: {error}") from error
    found = [
        path
        for path in extract.rglob("*")
        if path.is_file() and path.name in {"agy", "antigravity"}
    ]
    if not found:
        raise GroundTruthError(f"{archive} contained no agy/antigravity binary")
    binary = found[0]
    binary.chmod(0o755)
    alias = prefix / "agy"
    if binary.name != "agy":
        alias.unlink(missing_ok=True)
        alias.symlink_to(binary)
        return alias
    return binary


def _write_antigravity_settings(agent_home: Path) -> None:
    agent_home.mkdir(parents=True, exist_ok=True)
    (agent_home / "settings.json").write_text(
        json.dumps(
            {"modelProvider": "gemini", "toolPermission": "always-proceed"},
            indent=2,
        )
        + "\n"
    )


def _strip_antigravity_harvest(tree: Path) -> None:
    for name in ("bin", "builtin", "log", "updater", "crashes", "presence"):
        shutil.rmtree(tree / name, ignore_errors=True)
    (tree / "cli.log").unlink(missing_ok=True)
    (tree / "last_check.timestamp").unlink(missing_ok=True)
    changelog = tree / "cache" / "CHANGELOG.md"
    changelog.unlink(missing_ok=True)


def _write_grok_config(agent_home: Path, base_url: str) -> None:
    (agent_home / "config.toml").write_text(
        "[cli]\n"
        "auto_update = false\n"
        "\n"
        "[features]\n"
        "telemetry = false\n"
        "feedback = false\n"
        "codebase_indexing = false\n"
        "remote_fetch = false\n"
        "\n"
        "[memory]\n"
        "enabled = false\n"
        "\n"
        "[workflows]\n"
        "enabled = false\n"
        "\n"
        "[subagents]\n"
        "enabled = false\n"
        "\n"
        "[models]\n"
        'default = "mock-model"\n'
        "\n"
        "[model.mock-model]\n"
        'model = "mock-model"\n'
        f'base_url = "{base_url}/v1"\n'
        'name = "Rosetta Mock"\n'
        'env_key = "XAI_API_KEY"\n'
        'api_backend = "chat_completions"\n'
        "context_window = 128000\n"
    )


def _git_init(project: Path) -> None:
    _run(["git", "init", "-q"], cwd=project)
    _run(["git", "config", "user.email", "rosetta@mock"], cwd=project)
    _run(["git", "config", "user.name", "rosetta"], cwd=project)
    _run(["git", "config", "commit.gpgsign", "false"], cwd=project)
    _run(
        ["git", "commit", "--allow-empty", "-q", "-m", "rosetta-farm"],
        cwd=project,
    )


def _remove_empty_directories(root: Path) -> None:
    for path in sorted(root.rglob("*"), reverse=True):
        if path.is_dir() and not any(path.iterdir()):
            path.rmdir()


def _write_opencode_config(project: Path, base_url: str) -> None:
    write_json(
        project / "opencode.json",
        {
            "$schema": "https://opencode.ai/config.json",
            "model": "mock/mock-model",
            "provider": {
                "mock": {
                    "npm": "@ai-sdk/openai-compatible",
                    "name": "Rosetta mock",
                    "options": {
                        "baseURL": f"{base_url}/v1",
                        "apiKey": "rosetta-mock-key",
                    },
                    "models": {"mock-model": {"name": "mock-model"}},
                }
            },
        },
    )


def _write_pi_config(
    agent_home: Path,
    base_url: str,
    *,
    context_window: int = 128000,
    compact: bool = False,
) -> None:
    coding = agent_home / "agent"
    coding.mkdir(parents=True, exist_ok=True)
    write_json(
        coding / "models.json",
        {
            "providers": {
                "mock": {
                    "baseUrl": f"{base_url}/v1",
                    "api": "openai-completions",
                    "apiKey": "$MOCK_API_KEY",
                    "compat": {
                        "supportsDeveloperRole": False,
                        "supportsReasoningEffort": False,
                        "supportsStore": False,
                    },
                    "models": [
                        {
                            "id": "mock-model",
                            "name": "Rosetta Mock",
                            "reasoning": False,
                            "input": ["text"],
                            "contextWindow": context_window,
                            "maxTokens": 4096,
                            "cost": {
                                "input": 0,
                                "output": 0,
                                "cacheRead": 0,
                                "cacheWrite": 0,
                            },
                        }
                    ],
                }
            }
        },
    )
    settings: dict[str, Any] = {
        "enableInstallTelemetry": False,
        "defaultProjectTrust": "never",
        "retry": {"enabled": False},
    }
    if compact:
        settings["compaction"] = {
            "enabled": True,
            "reserveTokens": 128,
            "keepRecentTokens": 32,
        }
    write_json(coding / "settings.json", settings)


def _strip_grok_harvest(tree: Path) -> None:
    for name in (
        "auth.json",
        "auth.json.lock",
        "mcp_credentials.json",
        "mcp_credentials.json.lock",
    ):
        (tree / name).unlink(missing_ok=True)
    for name in (
        "bundled",
        "downloads",
        "vendor",
        "docs",
        "bin",
        "completions",
        "marketplace-cache",
        "installed-plugins",
        "worktrees",
        "crash",
        "skills",
        "plugins",
        "agents",
        "personas",
        "workflows",
        "relocations",
    ):
        shutil.rmtree(tree / name, ignore_errors=True)
    (tree / "README.md").unlink(missing_ok=True)
    (tree / "CHANGELOG.md").unlink(missing_ok=True)
    (tree / "CHANGELOG.json").unlink(missing_ok=True)
    for path in tree.rglob("*"):
        if path.is_file() and path.suffix in {".lock", ".sock"}:
            path.unlink(missing_ok=True)


HERMES_REPO = "https://github.com/NousResearch/hermes-agent.git"
HERMES_PYTHON = "3.13"


def _docker_available() -> bool:
    if shutil.which("docker") is None:
        return False
    try:
        subprocess.run(
            ["docker", "info"],
            check=True,
            capture_output=True,
            timeout=8,
        )
    except (OSError, subprocess.CalledProcessError, subprocess.TimeoutExpired):
        return False
    return True


def _hermes_release(watch: dict[str, Any], requested: str) -> tuple[str, str]:
    release = watch.get("release", {})
    git_tag = release.get("git_tag")
    if not isinstance(git_tag, str) or not git_tag:
        git_tag = requested if requested.startswith("v") else f"v{requested}"
    image = release.get("docker_image")
    if not isinstance(image, str) or not image:
        image = f"nousresearch/hermes-agent:{git_tag}"
    return git_tag, image


def _write_hermes_wrapper(prefix: Path, image: str) -> Path:
    (prefix / "image").write_text(image + "\n")
    wrapper = prefix / "hermes"
    wrapper.write_text(
        "#!/bin/sh\n"
        "set -e\n"
        'IMAGE=$(cat "$(dirname "$0")/image")\n'
        'HOME_DIR=${HERMES_FARM_HOME:?}\n'
        'PROJECT_DIR=${HERMES_FARM_PROJECT:?}\n'
        'FARM_DIR=${HERMES_FARM_SCENARIO:?}\n'
        f'UID_N=${{HERMES_UID:-{os.getuid()}}}\n'
        f'GID_N=${{HERMES_GID:-{os.getgid()}}}\n'
        "exec docker run --rm \\\n"
        '  -v "$HOME_DIR:/opt/data" \\\n'
        '  -v "$PROJECT_DIR:/workspace" \\\n'
        '  -v "$FARM_DIR:/farm" \\\n'
        "  -w /workspace \\\n"
        "  --add-host host.docker.internal:host-gateway \\\n"
        "  -e HERMES_HOME=/opt/data \\\n"
        '  -e HERMES_UID="$UID_N" \\\n'
        '  -e HERMES_GID="$GID_N" \\\n'
        '  "$IMAGE" \\\n'
        '  "$@"\n'
    )
    wrapper.chmod(0o755)
    return wrapper


def _install_hermes(prefix: Path, watch: dict[str, Any], requested: str) -> Path:
    prefix.mkdir(parents=True, exist_ok=True)
    git_tag, image = _hermes_release(watch, requested)
    if _docker_available():
        print(f"pulling {image} …")
        _run(["docker", "pull", image])
        return _write_hermes_wrapper(prefix, image)

    src = prefix / "src"
    print(f"cloning {HERMES_REPO}@{git_tag} …")
    _run(
        [
            "git",
            "clone",
            "--depth",
            "1",
            "--branch",
            git_tag,
            HERMES_REPO,
            str(src),
        ]
    )
    if shutil.which("uv") is None:
        raise GroundTruthError("hermes farm requires docker or uv to install the pinned release")
    print(f"uv sync hermes-agent {git_tag} (CPython {HERMES_PYTHON}) …")
    environment = {**os.environ, "UV_NO_CONFIG": "1", "UV_PYTHON": HERMES_PYTHON}
    _run(["uv", "python", "install", HERMES_PYTHON], env=environment)
    _run(
        ["uv", "sync", "--frozen", "--no-dev"],
        cwd=src,
        env=environment,
    )
    binary = src / ".venv" / "bin" / "hermes"
    if not binary.is_file():
        raise GroundTruthError(f"hermes binary missing after uv sync: {binary}")
    return binary


def _write_hermes_config(agent_home: Path, api_base: str) -> None:
    (agent_home / "config.yaml").write_text(
        "model:\n"
        "  default: mock-model\n"
        "  provider: custom\n"
        f"  base_url: {api_base}\n"
        "  api_key: rosetta-mock-key\n"
        "  context_length: 128000\n"
        "terminal:\n"
        "  backend: local\n"
        "  timeout: 30\n"
        "memory:\n"
        "  memory_enabled: false\n"
        "  user_profile_enabled: false\n"
        "compression:\n"
        "  enabled: false\n"
        "agent:\n"
        "  max_turns: 8\n"
        "  disabled_toolsets:\n"
        "    - web\n"
        "    - browser\n"
        "    - memory\n"
        "auxiliary:\n"
        "  compression:\n"
        "    provider: main\n"
        "  vision:\n"
        "    provider: main\n"
    )
    (agent_home / ".env").write_text("OPENAI_API_KEY=rosetta-mock-key\n")


def _strip_hermes_harvest(tree: Path) -> None:
    (tree / ".env").unlink(missing_ok=True)
    (tree / "auth.json").unlink(missing_ok=True)
    (tree / ".update_check").unlink(missing_ok=True)
    for name in (
        "skills",
        "cache",
        "node",
        "bin",
        "plugins",
        "pending",
        "backups",
        "home",
        "hermes-agent",
        "logs",
        "cron",
    ):
        shutil.rmtree(tree / name, ignore_errors=True)


def _dump_sqlite_jsonl(database: Path, destination: Path) -> None:
    if not database.is_file():
        return
    connection = sqlite3.connect(database)
    connection.row_factory = sqlite3.Row
    try:
        tables = [
            row[0]
            for row in connection.execute(
                "SELECT name FROM sqlite_master "
                "WHERE type='table' AND name NOT LIKE 'sqlite_%' "
                "AND name NOT LIKE '%_fts%' ORDER BY name"
            )
        ]
        for table in tables:
            rows = connection.execute(f"SELECT * FROM {_quote_identifier(table)}").fetchall()
            if not rows:
                continue
            path = destination / f"{table}.jsonl"
            with path.open("w") as output:
                for row in rows:
                    output.write(json.dumps(dict(row), ensure_ascii=False, default=str) + "\n")
    except sqlite3.Error as error:
        raise GroundTruthError(f"exporting {database}: {error}") from error
    finally:
        connection.close()


def _hermes_latest_session(agent_home: Path) -> str | None:
    database = agent_home / "state.db"
    if not database.is_file():
        return None
    connection = sqlite3.connect(database)
    try:
        row = connection.execute(
            "SELECT id FROM sessions ORDER BY started_at DESC LIMIT 1"
        ).fetchone()
    except sqlite3.Error:
        return None
    finally:
        connection.close()
    return row[0] if row and isinstance(row[0], str) else None


def _parse_agent_version(output: str) -> str:
    # Require a three-part semver. A looser `\b\d+(?:\.\d+)+` match turns
    # Hermes' `v0.20.5` into `20.5` because `v0` is one word.
    match = re.search(r"(?i)(?:^|\s)v(\d+\.\d+\.\d+\S*)", output)
    if match is None:
        match = re.search(r"\b(\d+\.\d+\.\d+\S*)", output)
    if match is None:
        match = re.search(r"\b(\d+\.\d+[^\s]*)", output)
    if match is None:
        raise GroundTruthError(f"could not detect installed version from {output!r}")
    return match.group(1).rstrip(".,;:")


def _detect_version(binary: Path) -> str:
    if not binary.exists():
        raise GroundTruthError(f"binary not found at {binary}")
    try:
        output = subprocess.run(
            [str(binary), "--version"], check=True, capture_output=True, text=True
        ).stdout
    except (OSError, subprocess.CalledProcessError) as error:
        raise GroundTruthError(f"could not run {binary} --version: {error}") from error
    return _parse_agent_version(output)


def _detect_hermes_version(binary: Path) -> str:
    image_file = binary.parent / "image"
    if image_file.is_file():
        image = image_file.read_text().strip()
        try:
            result = subprocess.run(
                ["docker", "run", "--rm", image, "--version"],
                check=True,
                capture_output=True,
                text=True,
                timeout=120,
            )
        except (OSError, subprocess.CalledProcessError, subprocess.TimeoutExpired) as error:
            raise GroundTruthError(f"could not run {image} --version: {error}") from error
        output = result.stdout or result.stderr
        return _parse_agent_version(output)
    return _detect_version(binary)


def _scenario_step(
    command: list[str],
    scenario_dir: Path,
    scenario: str,
    step: int,
    env: dict[str, str],
    project: Path,
    *,
    allow_failure: bool = False,
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
    if result.returncode and not allow_failure:
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


def _redact(
    destination: Path,
    scenario_dir: Path,
    work: Path,
    project: Path | None = None,
) -> None:
    replacements = [
        (str(scenario_dir), "<SCENARIO_DIR>"),
        (f"/private{scenario_dir}", "<SCENARIO_DIR>"),
        (str(work), "<WORK>"),
        (f"/private{work}", "<WORK>"),
    ]
    if project is not None:
        resolved = project.resolve()
        replacements.extend(
            [
                (str(resolved), "<PROJECT>"),
                (str(project), "<PROJECT>"),
                (f"/private{resolved}", "<PROJECT>"),
                (f"/private{project}", "<PROJECT>"),
                # OpenCode stores some paths without a leading slash (`private/tmp/...`).
                (f"private{resolved}", "<PROJECT>"),
                (f"private{project}", "<PROJECT>"),
            ]
        )
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
        if _is_sqlite_database(path):
            _redact_sqlite(path, replacements)
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


def refresh_fixture_manifests(fixture_root: Path) -> None:
    """Re-inventory scenarios after readers have closed any SQLite WAL files."""

    for manifest_path in sorted(fixture_root.glob("*/manifest.json")):
        try:
            manifest = json.loads(manifest_path.read_text())
            _manifest(
                manifest_path.parent,
                str(manifest["agent"]),
                str(manifest["agent_version"]),
                str(manifest["scenario"]),
                str(manifest["mock_dialect"]),
            )
        except (OSError, json.JSONDecodeError, KeyError) as error:
            raise GroundTruthError(f"refreshing {manifest_path}: {error}") from error


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


def _scenario_invocation(
    agent: str,
    binary: Path,
    prompt: str,
    step: int,
    extra_args: tuple[str, ...],
    home: Path,
    agent_home: Path,
    base_url: str,
    project: Path | None = None,
    scenario_dir: Path | None = None,
) -> tuple[list[str], dict[str, str], str]:
    if agent == "claude-code":
        command = [
            str(binary),
            "-p",
            prompt,
            "--output-format",
            "stream-json",
            "--verbose",
            "--bare",
            *extra_args,
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
        return command, environment, "anthropic-messages"
    if agent == "codex":
        command = [str(binary), "exec"]
        if step:
            command.extend(["resume", "--last"])
        command.extend([prompt, "--json", "--skip-git-repo-check", *extra_args])
        environment = _minimal_env(
            HOME=home,
            CODEX_HOME=agent_home,
            MOCK_API_KEY="rosetta-mock-key",
        )
        return command, environment, "openai-responses"
    if agent == "kimi-code":
        command = [
            str(binary),
            "-p",
            prompt,
            "--output-format",
            "stream-json",
            *extra_args,
        ]
        if step:
            command.append("-c")
        environment = _minimal_env(
            HOME=home,
            KIMI_CODE_HOME=agent_home,
            KIMI_MODEL_NAME="mock-model",
            KIMI_MODEL_API_KEY="rosetta-mock-key",
            KIMI_MODEL_BASE_URL=f"{base_url}/v1",
            KIMI_MODEL_PROVIDER_TYPE="openai",
            KIMI_DISABLE_TELEMETRY="1",
            KIMI_CODE_NO_AUTO_UPDATE="1",
            KIMI_DISABLE_CRON="1",
            CI="1",
        )
        return command, environment, "openai-chat"
    if agent == "grok":
        command = [
            str(binary),
            "-p",
            prompt,
            "-m",
            "mock-model",
            "--output-format",
            "streaming-json",
            "--no-auto-update",
            "--yolo",
            "--disable-web-search",
            "--no-plan",
            "--no-subagents",
            *extra_args,
        ]
        if step:
            command.append("-c")
        environment = _minimal_env(
            PATH=f"{binary.parent}{os.pathsep}{os.environ.get('PATH', '')}",
            HOME=home,
            GROK_HOME=agent_home,
            XAI_API_KEY="rosetta-mock-key",
            GROK_DISABLE_AUTOUPDATER="1",
            GROK_TELEMETRY_ENABLED="0",
            GROK_MEMORY="0",
            GROK_WORKFLOWS="0",
            GROK_SUBAGENTS="0",
            GROK_FEEDBACK_ENABLED="0",
            GROK_WEB_FETCH="0",
            GROK_MODELS_BASE_URL=f"{base_url}/v1",
        )
        return command, environment, "openai-chat"
    if agent == "copilot":
        command = [
            str(binary),
            "-p",
            prompt,
            "--output-format",
            "json",
            "--no-ask-user",
            *extra_args,
        ]
        if scenario_dir is not None:
            scenario_dir.mkdir(parents=True, exist_ok=True)
            command.extend(["--share", str(scenario_dir / "share.md")])
        if step:
            command.append("--continue")
        environment = _minimal_env(
            HOME=home,
            COPILOT_HOME=agent_home,
            COPILOT_CACHE_HOME=home / "cache",
            COPILOT_PROVIDER_BASE_URL=f"{base_url}/v1",
            COPILOT_PROVIDER_TYPE="openai",
            COPILOT_PROVIDER_API_KEY="rosetta-mock-key",
            COPILOT_MODEL="mock-model",
            COPILOT_OFFLINE="true",
            CI="1",
        )
        if "--allow-all" in extra_args:
            environment["COPILOT_ALLOW_ALL"] = "true"
        return command, environment, "openai-chat"
    if agent == "pi":
        command = [
            str(binary),
            "-p",
            "--mode",
            "json",
            "--provider",
            "mock",
            "--model",
            "mock-model",
            "--thinking",
            "off",
            "--no-extensions",
            "--no-skills",
            "--no-prompt-templates",
            "--no-themes",
            "--no-context-files",
            "--offline",
            *extra_args,
        ]
        if step and "--fork" not in extra_args:
            command.insert(1, "-c")
        command.append(prompt)
        environment = _minimal_env(
            HOME=home,
            PI_CODING_AGENT_DIR=agent_home / "agent",
            MOCK_API_KEY="rosetta-mock-key",
            PI_OFFLINE="1",
            PI_SKIP_VERSION_CHECK="1",
            PI_TELEMETRY="0",
        )
        return command, environment, "openai-chat"
    if agent == "opencode":
        command = [
            str(binary),
            "run",
            prompt,
            "-m",
            "mock/mock-model",
            "--format",
            "json",
            "--auto",
            *extra_args,
        ]
        if step:
            command.append("-c")
        environment = _minimal_env(
            HOME=home,
            XDG_DATA_HOME=home / ".local" / "share",
            XDG_CONFIG_HOME=home / ".config",
            XDG_CACHE_HOME=home / ".cache",
            XDG_STATE_HOME=home / ".local" / "state",
            OPENCODE_DB=str(agent_home / "opencode.db"),
            OPENCODE_DISABLE_AUTOUPDATE="1",
            OPENCODE_DISABLE_MODELS_FETCH="1",
            OPENCODE_DISABLE_TERMINAL_TITLE="1",
        )
        return command, environment, "openai-chat"
    if agent == "antigravity":
        command = [str(binary)]
        if step:
            command.append("-c")
        command.extend(
            [
                "-p",
                prompt,
                "--output-format",
                "stream-json",
                "--print-timeout",
                "2m",
                "--dangerously-skip-permissions",
                *extra_args,
            ]
        )
        environment = _minimal_env(
            PATH=f"{binary.parent}{os.pathsep}{os.environ.get('PATH', '')}",
            HOME=home,
            XDG_CONFIG_HOME=home / ".config",
            XDG_CACHE_HOME=home / ".cache",
            XDG_DATA_HOME=home / ".local" / "share",
            GEMINI_API_KEY="rosetta-mock-key",
            GOOGLE_GEMINI_BASE_URL=base_url,
            AGY_CLI_DISABLE_AUTO_UPDATE="1",
            AGY_CLI_HIDE_ACCOUNT_INFO="1",
        )
        return command, environment, "gemini-generatecontent"
    if agent == "hermes":
        if project is None or scenario_dir is None:
            raise GroundTruthError("hermes farm needs project and scenario_dir")
        docker = (binary.parent / "image").is_file()
        prompt_name = f"prompt-{step}.txt"
        (scenario_dir / prompt_name).write_text(prompt)
        query = f"/farm/{prompt_name}" if docker else str(scenario_dir / prompt_name)
        command = [
            str(binary),
            "chat",
            "--query-file",
            query,
            "--yolo",
            "--quiet",
            "--ignore-rules",
            "--toolsets",
            "terminal",
            "--max-turns",
            "8",
            *extra_args,
        ]
        if step:
            session = _hermes_latest_session(agent_home)
            if session:
                command.extend(["--resume", session])
            else:
                command.append("--continue")
        environment = _minimal_env(
            HOME=home,
            HERMES_HOME=agent_home if not docker else "/opt/data",
            HERMES_FARM_HOME=agent_home,
            HERMES_FARM_PROJECT=project,
            HERMES_FARM_SCENARIO=scenario_dir,
            OPENAI_API_KEY="rosetta-mock-key",
        )
        return command, environment, "openai-chat"
    raise GroundTruthError(f"no farm invocation for {agent}")


def _strip_harvest(agent: str, harvested: Path) -> None:
    if agent == "claude-code":
        shutil.rmtree(harvested / "backups", ignore_errors=True)
        return
    if agent == "codex":
        (harvested / "auth.json").unlink(missing_ok=True)
        shutil.rmtree(harvested / ".tmp", ignore_errors=True)
        shutil.rmtree(harvested / "skills", ignore_errors=True)
        return
    if agent == "kimi-code":
        shutil.rmtree(harvested / "credentials", ignore_errors=True)
        shutil.rmtree(harvested / "oauth", ignore_errors=True)
        shutil.rmtree(harvested / "bin", ignore_errors=True)
        shutil.rmtree(harvested / "updates", ignore_errors=True)
        shutil.rmtree(harvested / "skills", ignore_errors=True)
        shutil.rmtree(harvested / "plugins" / "managed", ignore_errors=True)
        shutil.rmtree(harvested / "cache", ignore_errors=True)
        shutil.rmtree(harvested / "telemetry", ignore_errors=True)
        shutil.rmtree(harvested / "search-index", ignore_errors=True)
        shutil.rmtree(harvested / "server", ignore_errors=True)
        (harvested / "server.token").unlink(missing_ok=True)
        return
    if agent == "grok":
        _strip_grok_harvest(harvested)
        return
    if agent == "copilot":
        for name in ("session-store.db", "session-store.db-wal", "session-store.db-shm"):
            (harvested / name).unlink(missing_ok=True)
        shutil.rmtree(harvested / "logs", ignore_errors=True)
        shutil.rmtree(harvested / "pkg", ignore_errors=True)
        shutil.rmtree(harvested / "installed-plugins", ignore_errors=True)
        for lock in harvested.rglob("inuse.*.lock"):
            lock.unlink(missing_ok=True)
        return
    if agent == "pi":
        coding = harvested / "agent"
        (coding / "auth.json").unlink(missing_ok=True)
        shutil.rmtree(coding / "npm", ignore_errors=True)
        shutil.rmtree(coding / "git", ignore_errors=True)
        shutil.rmtree(coding / "logs", ignore_errors=True)
        shutil.rmtree(coding / "bin", ignore_errors=True)
        return
    if agent == "opencode":
        (harvested / "auth.json").unlink(missing_ok=True)
        for hooks in harvested.glob("snapshot/*/*/hooks"):
            shutil.rmtree(hooks, ignore_errors=True)
        _remove_empty_directories(harvested)
        return
    if agent == "antigravity":
        _strip_antigravity_harvest(harvested)
        return
    if agent == "hermes":
        _strip_hermes_harvest(harvested)
        return
    raise GroundTruthError(f"no harvest stripper for {agent}")


def _assert_expected_record(
    agent_home: Path,
    layout: dict[str, Any],
    stream_id: str,
    record_type: str,
) -> None:
    for path in sorted(agent_home.rglob("*.jsonl")):
        relative = path.relative_to(agent_home).as_posix()
        stream = matching_stream(layout, relative)
        if stream is None or stream["id"] != stream_id:
            continue
        try:
            lines = path.read_text().splitlines()
        except (OSError, UnicodeDecodeError):
            continue
        for line in lines:
            try:
                value = json.loads(line)
            except json.JSONDecodeError:
                continue
            if jsonl_record_type(value) == record_type:
                return
    raise GroundTruthError(
        f"scenario contract failed: {stream_id} emitted no {record_type!r} record"
    )


def _generate_fixtures(
    agent: str,
    agent_dir: Path,
    requested: str,
    keep_work: bool,
    scenario_name: str | None = None,
) -> str:
    watch = _load_watch(agent_dir)
    layout = load_layout(agent_dir)
    work = Path(tempfile.gettempdir()) / f"rosetta-farm-{agent}-{os.getpid()}"
    if work.exists():
        shutil.rmtree(work)
    work.mkdir(parents=True)
    print(f"work dir: {work}")

    with _farm_lock():
        install = work / "install"
        mock_log = work / "mockmodel.jsonl"
        if agent == "grok":
            binary = _install_grok(install, requested)
            actual = _detect_version(binary)
        elif agent == "antigravity":
            binary = _install_antigravity(install, requested)
            actual = _detect_version(binary)
        elif agent == "hermes":
            binary = _install_hermes(install, watch, requested)
            actual = _detect_hermes_version(binary)
        else:
            package = watch["release"]["id"]
            if agent == "codex" and "/" in package and not package.startswith("@"):
                package = "@openai/codex"
            _npm_install(install, package, requested, ignore_scripts=agent == "pi")

            if agent == "claude-code":
                _run(
                    [
                        "node",
                        str(install / "node_modules/@anthropic-ai/claude-code/install.cjs"),
                    ]
                )
            if agent == "opencode":
                # npm may skip this package's postinstall even without
                # --ignore-scripts; the stub bin is not a real executable.
                _run(
                    [
                        "node",
                        str(install / "node_modules/opencode-ai/postinstall.mjs"),
                    ]
                )
            binary = install / "node_modules/.bin" / BINARIES[agent]
            actual = _detect_version(binary)
        print(f"installed version: {actual}")
        if agent == "copilot":
            schema_doc = write_copilot_writer_schema(install, agent_dir, actual)
            print(f"writer schema -> {schema_doc}")

        server, thread = start_server(log_path=mock_log)
        host, port = server.server_address
        base_url = f"http://{host}:{port}"
        print(f"mockmodel at {base_url}")
        try:
            fixture_root = agent_dir / "fixtures" / actual
            scenarios = [
                scenario
                for scenario in SCENARIOS[agent]
                if scenario_name is None or scenario["name"] == scenario_name
            ]
            if not scenarios:
                raise GroundTruthError(
                    f"unknown scenario {scenario_name!r} for {agent}; choose from "
                    + ", ".join(scenario["name"] for scenario in SCENARIOS[agent])
                )
            for scenario in scenarios:
                if not thread.is_alive():
                    raise GroundTruthError(
                        f"mockmodel died before scenario {scenario['name']}"
                    )
                name = scenario["name"]
                print(f"scenario: {name}")
                scenario_dir = work / "scenarios" / name
                home = scenario_dir / "home"
                tree_name = TREE_NAMES[agent]
                project = _clean_project(name)
                home.mkdir(parents=True)
                if agent == "opencode":
                    agent_home = home / ".local" / "share" / "opencode"
                elif agent == "antigravity":
                    agent_home = home / ".gemini" / "antigravity-cli"
                else:
                    agent_home = scenario_dir / tree_name
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
                elif agent == "copilot":
                    (agent_home / "settings.json").write_text(
                        json.dumps(
                            {
                                "autoUpdate": False,
                                "remoteExport": False,
                                "banner": "never",
                                "beep": False,
                            },
                            indent=2,
                        )
                        + "\n"
                    )
                elif agent == "grok":
                    _write_grok_config(agent_home, base_url)
                elif agent == "pi":
                    _write_pi_config(
                        agent_home,
                        base_url,
                        context_window=1024 if name == "compaction" else 128000,
                        compact=name == "compaction",
                    )
                elif agent == "opencode":
                    _write_opencode_config(project, base_url)
                    # Keep one scenario on the non-git `global` project so both
                    # project shapes are fixture-backed.
                    if name != "basic-chat":
                        _git_init(project)
                elif agent == "antigravity":
                    _write_antigravity_settings(agent_home)
                elif agent == "hermes":
                    api_base = f"{base_url}/v1"
                    if (binary.parent / "image").is_file():
                        api_base = (
                            f"http://host.docker.internal:{port}/v1"
                        )
                    _write_hermes_config(agent_home, api_base)

                for step, prompt in enumerate(scenario["prompts"]):
                    extra_args = list(scenario["extra_args"])
                    if agent == "pi" and name == "fork" and step:
                        sessions = sorted(
                            (agent_home / "agent" / "sessions").rglob("*.jsonl")
                        )
                        if not sessions:
                            raise GroundTruthError(
                                "pi fork scenario: no session file to fork"
                            )
                        extra_args = ["--fork", str(sessions[0]), *extra_args]
                    command, environment, dialect = _scenario_invocation(
                        agent,
                        binary,
                        prompt,
                        step,
                        tuple(extra_args),
                        home,
                        agent_home,
                        base_url,
                        project=project,
                        scenario_dir=scenario_dir,
                    )
                    _scenario_step(
                        command,
                        scenario_dir,
                        name,
                        step,
                        environment,
                        project,
                        allow_failure=bool(scenario.get("allow_failure")),
                    )

                if scenario.get("expected_record"):
                    stream_id, record_type = scenario["expected_record"]
                    _assert_expected_record(
                        agent_home, layout, stream_id, record_type
                    )

                destination = fixture_root / name
                _harvest(agent_home, tree_name, scenario_dir, destination)
                _strip_harvest(agent, destination / tree_name)
                if agent == "hermes":
                    _dump_sqlite_jsonl(destination / tree_name / "state.db", destination)
                _redact(destination, scenario_dir, work, project)
                _manifest(destination, agent, actual, name, dialect)
                print(f"harvested -> {destination}")
        except GroundTruthError:
            if mock_log.is_file():
                tail = mock_log.read_text().splitlines()[-20:]
                print("mockmodel log tail:\n" + "\n".join(tail))
            raise
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=5)

    if keep_work:
        print(f"work dir kept: {work}")
    else:
        shutil.rmtree(work, ignore_errors=True)
    return actual


def _is_sqlite_database(path: Path) -> bool:
    name = path.name
    if name.endswith(("-wal", "-shm")):
        return False
    return path.suffix in {".sqlite", ".db"}


def _redact_sqlite(path: Path, replacements: list[tuple[str, str]]) -> None:
    try:
        connection = sqlite3.connect(path)
        tables = [
            row[0]
            for row in connection.execute(
                "SELECT name FROM sqlite_master "
                "WHERE type='table' AND name NOT LIKE 'sqlite_%'"
            )
        ]
        for table in tables:
            columns = connection.execute(
                f"PRAGMA table_info({_quote_identifier(table)})"
            ).fetchall()
            text_columns = [
                name
                for _, name, declared_type, *_ in columns
                if any(
                    token in declared_type.upper()
                    for token in ("CHAR", "CLOB", "TEXT", "JSON")
                )
                or not declared_type
            ]
            quoted_table = _quote_identifier(table)
            for column in text_columns:
                quoted = _quote_identifier(column)
                for needle, replacement in replacements:
                    connection.execute(
                        f"UPDATE {quoted_table} SET {quoted} = replace({quoted}, ?, ?) "
                        f"WHERE {quoted} IS NOT NULL AND instr({quoted}, ?) > 0",
                        (needle, replacement, needle),
                    )
        connection.commit()
        # Fold redactions into the main file so the committed DB is complete,
        # but leave journal_mode=WAL — that is the writer's on-disk shape.
        connection.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    except sqlite3.Error as error:
        raise GroundTruthError(f"redacting {path}: {error}") from error
    finally:
        if "connection" in locals():
            connection.close()


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


def _record_facet(
    facet_accumulators: dict[tuple[str, str], tuple[str, str, ShapeAccumulator]],
    stream_id: str,
    hit: dict[str, Any],
) -> None:
    key = (stream_id, hit["facet_id"])
    if key not in facet_accumulators:
        facet_accumulators[key] = (hit["path"], hit["discriminator"], ShapeAccumulator())
    facet_accumulators[key][2].absorb(hit["key"], hit["value"])


def _absorb_sqlite(
    accumulator: ShapeAccumulator,
    database_name: str,
    path: Path,
    schemas: dict[str, list[str]],
    *,
    agent: str = "",
    stream_id: str = "",
    facet_accumulators: dict[tuple[str, str], tuple[str, str, ShapeAccumulator]]
    | None = None,
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
            column_names = [name for _, name, *_ in columns]
            json_shapes: dict[str, dict[str, Any]] = {}
            context: dict[str, str] = {}
            skip_content = table in {
                "account",
                "account_state",
                "control_account",
                "credential",
            }
            if rows and column_names and not skip_content:
                for row in connection.execute(
                    f"SELECT * FROM {_quote_identifier(table)}"
                ):
                    row_map = dict(zip(column_names, row))
                    for name, value in row_map.items():
                        if not isinstance(value, str):
                            continue
                        stripped = value.lstrip()
                        if not stripped or stripped[0] not in "{[":
                            continue
                        try:
                            parsed = json.loads(value)
                        except json.JSONDecodeError:
                            continue
                        if not isinstance(parsed, (dict, list)):
                            continue
                        incoming = shape_of(parsed)
                        json_shapes[name] = (
                            incoming
                            if name not in json_shapes
                            else merge_shapes(json_shapes[name], incoming)
                        )
                        if agent and stream_id and facet_accumulators is not None:
                            payload = {
                                "table": table,
                                "column": name,
                                "data": parsed,
                            }
                            event_type = row_map.get("type")
                            if table == "event" and isinstance(event_type, str):
                                payload["event_type"] = event_type
                            role = row_map.get("role")
                            if isinstance(role, str):
                                payload["role"] = role
                            tool_name = row_map.get("tool_name")
                            if isinstance(tool_name, str):
                                payload["tool_name"] = tool_name
                            for hit in extract_facets(
                                agent, stream_id, payload, context
                            ):
                                _record_facet(facet_accumulators, stream_id, hit)
            for name, shape in json_shapes.items():
                fields[name]["shape"] = shape
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
    return jsonl_record_type(value)


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
        elif (
            len(fixture_parts) == 2
            and path.suffix == ".jsonl"
            and path.name != "manifest.json"
        ):
            if path.name.startswith("stdout"):
                stream_id = "farm:headless-stdout"
            else:
                stream_id = f"farm:{path.stem}"
            is_sqlite = False
        else:
            continue

        if is_sqlite:
            if _is_sqlite_database(path):
                _absorb_sqlite(
                    accumulators[stream_id],
                    sqlite_schema_name(path, relative),
                    path,
                    sqlite_schemas,
                    agent=layout["agent"],
                    stream_id=stream_id,
                    facet_accumulators=facet_accumulators,
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
                record_kind = _record_type(value)
                if stream_id.startswith("farm:") and not path.name.startswith("stdout"):
                    record_kind = path.stem
                elif stream_id in {"session-index", "user-history"}:
                    record_kind = ""
                accumulators[stream_id].absorb(record_kind, value)
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
            if spec["id"] == "updates":
                stream["discriminator"] = "sessionUpdate"
            elif spec["id"] not in {
                "prompt-history",
                "rewind-points",
                "unified-log",
                "session-index",
                "user-history",
            }:
                stream["discriminator"] = "type"
        elif spec["file_format"] == "sqlite" and record_types:
            stream["discriminator"] = "table"
        if record_types:
            stream["record_types"] = record_types
        if facets_by_stream.get(spec["id"]):
            stream["facets"] = facets_by_stream[spec["id"]]
        streams.append(stream)

    leftover = {
        stream_id: accumulators.pop(stream_id)
        for stream_id in list(accumulators)
        if stream_id.startswith("farm:")
    }
    for stream_id, accumulator in sorted(leftover.items()):
        stem = stream_id.split(":", 1)[1]
        if stream_id == "farm:headless-stdout":
            glob = "<farm>/stdout*.jsonl"
            notes = (
                "Headless stream-json stdout — corroboration surface captured "
                "by the farm, not part of the agent data root."
            )
            discriminator = "event" if layout["agent"] == "antigravity" else "type"
        else:
            glob = f"<farm>/{stem}.jsonl"
            notes = (
                "Farm-generated corroboration at the scenario root, not part "
                "of the agent data root."
            )
            discriminator = "table"
        stream = {
            "id": stream_id,
            "glob": glob,
            "file_format": "jsonl",
            "discipline": "append_only",
            "evidence": "fixture",
            "notes": notes,
            "discriminator": discriminator,
            "record_types": accumulator.into_types(),
        }
        if facets_by_stream.get(stream_id):
            stream["facets"] = facets_by_stream[stream_id]
        streams.append(stream)

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
    refresh_fixture_manifests(fixture_root)
    print(f"canonical ground truth -> {canonical}")
    print(f"schema report -> {report}")
    return canonical, report


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("agent", choices=sorted(SCENARIOS))
    parser.add_argument("--version")
    parser.add_argument("--keep-work", action="store_true")
    parser.add_argument("--schemas-only", action="store_true")
    parser.add_argument("--scenario", help="regenerate only one named scenario")
    parser.add_argument(
        "--allow-host-execution",
        action="store_true",
        help="dangerous: install and execute downloaded agent code outside an isolated runner",
    )
    args = parser.parse_args(argv)
    try:
        if args.schemas_only and args.scenario:
            raise GroundTruthError("--scenario cannot be used with --schemas-only")
        root = repo_root()
        agent_dir = root / "groundtruth" / args.agent
        watch = _load_watch(agent_dir)
        requested = _selected_version(
            watch, args.version, schemas_only=args.schemas_only
        )
        if not args.schemas_only:
            _require_isolated_runner(allow_host_execution=args.allow_host_execution)
        version = (
            requested
            if args.schemas_only
            else _generate_fixtures(
                args.agent,
                agent_dir,
                requested,
                args.keep_work,
                args.scenario,
            )
        )
        emit_groundtruth(agent_dir, version)
    except GroundTruthError as error:
        parser.error(str(error))
    print("done. Review the fixture, canonical JSON, and generated HTML diffs.")
    return 0
