"""Sweep `rosettad serve` starter-slice budgets over one frozen home snapshot."""

from __future__ import annotations

import argparse
import csv
import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
from collections import deque
from dataclasses import dataclass, field
from pathlib import Path
from typing import Iterable, Sequence

MILESTONE_COLUMN = {
    "starter-drained": "starter_s",
    "cold-drained": "cold_s",
    "canonical-ready": "canonical_s",
    "search-ready": "search_s",
}
MILESTONES = tuple(MILESTONE_COLUMN)
COLUMNS = ("budget", *MILESTONE_COLUMN.values(), "wall_s", "files", "paused", "ok")
RIGHT_ALIGNED = frozenset(COLUMNS[1:-1])
COUNTERS = ("files", "paused")
SUFFIXES = {"k": 1024, "m": 1024 * 1024}

DEFAULT_BUDGETS = "64k,128k,256k,512k,1m,0"
DEFAULT_TIMEOUT = 3600.0
GRACE_SECONDS = 10.0
TAIL_LINES = 40
MISSING = "—"

_MILESTONE_RE = re.compile(r"milestone:\s+(?P<name>[a-z][a-z-]*)(?P<rest>\s.*|$)")
_PAIR_RE = re.compile(r"(?P<key>[a-z_]+)=(?P<value>\S+)")


class SweepError(Exception):
    """A usage or environment error that must stop the sweep before it runs."""


def parse_budget(text: str) -> int:
    """Resolve one budget token (`0`, `65536`, `64k`, `1m`) to a byte count."""

    token = text.strip()
    if not token:
        raise SweepError("empty budget")
    scale = 1
    if token[-1].lower() in SUFFIXES:
        scale = SUFFIXES[token[-1].lower()]
        token = token[:-1]
    if not (token.isascii() and token.isdigit()):
        raise SweepError(
            f"bad budget {text!r}: want a whole number of bytes, "
            "optionally suffixed with k or m"
        )
    return int(token) * scale


def parse_budgets(text: str) -> list[tuple[str, int]]:
    """Split a comma-separated budget list into (token, bytes), input order kept."""

    budgets: list[tuple[str, int]] = []
    seen: dict[int, str] = {}
    for raw in text.split(","):
        token = raw.strip()
        value = parse_budget(token)
        previous = seen.get(value)
        if previous is not None:
            raise SweepError(
                f"duplicate budget {token!r} repeats {previous!r} ({value} bytes)"
            )
        seen[value] = token
        budgets.append((token, value))
    return budgets


def parse_milestone_line(line: str) -> tuple[str, dict[str, float]] | None:
    """Parse one `milestone: ...` line, or None when the line is unrelated noise."""

    match = _MILESTONE_RE.search(line)
    if match is None or match.group("name") not in MILESTONE_COLUMN:
        return None
    fields: dict[str, float] = {}
    for pair in _PAIR_RE.finditer(match.group("rest")):
        key, value = pair.group("key"), pair.group("value")
        if key == "elapsed_s":
            try:
                fields[key] = float(value)
            except ValueError:
                return None
        elif key in COUNTERS:
            if not (value.isascii() and value.isdigit()):
                return None
            fields[key] = int(value)
    if "elapsed_s" not in fields:
        return None
    return match.group("name"), fields


def parse_milestones(text: str) -> dict[str, dict[str, float]]:
    """Collect the first occurrence of each milestone from merged serve output."""

    found: dict[str, dict[str, float]] = {}
    for line in text.splitlines():
        parsed = parse_milestone_line(line)
        if parsed is not None:
            found.setdefault(parsed[0], parsed[1])
    return found


@dataclass
class RunResult:
    """One completed (or abandoned) serve run at a single budget."""

    budget: str
    milestones: dict[str, dict[str, float]] = field(default_factory=dict)
    wall_s: float = 0.0
    ok: bool = False
    status: str = "not run"
    tail: list[str] = field(default_factory=list)
    log_path: Path | None = None
    work_dir: Path | None = None


def _seconds(value: float | None) -> str:
    return "" if value is None else f"{value:.2f}"


def csv_row(result: RunResult) -> list[str]:
    """One CSV record; absent milestones and counters render as empty cells."""

    row = [result.budget]
    for name in MILESTONES:
        fields = result.milestones.get(name)
        row.append(_seconds(None if fields is None else fields["elapsed_s"]))
    row.append(f"{result.wall_s:.2f}")
    starter = result.milestones.get("starter-drained", {})
    for counter in COUNTERS:
        value = starter.get(counter)
        row.append("" if value is None else str(int(value)))
    row.append("true" if result.ok else "false")
    return row


def format_table(results: Iterable[RunResult]) -> str:
    """Render the results as a compact, column-aligned markdown table."""

    rows = [[cell or MISSING for cell in csv_row(result)] for result in results]
    widths = [
        max(len(column), *(len(row[index]) for row in rows)) if rows else len(column)
        for index, column in enumerate(COLUMNS)
    ]

    def render(cells: Sequence[str]) -> str:
        padded = [
            cell.rjust(widths[index])
            if COLUMNS[index] in RIGHT_ALIGNED
            else cell.ljust(widths[index])
            for index, cell in enumerate(cells)
        ]
        return "| " + " | ".join(padded) + " |"

    rule = [
        "-" * (widths[index] - 1) + ":"
        if column in RIGHT_ALIGNED
        else "-" * widths[index]
        for index, column in enumerate(COLUMNS)
    ]
    return "\n".join([render(COLUMNS), render(rule), *(render(row) for row in rows)])


def build_command(
    budget: str,
    *,
    db_path: Path,
    home: Path,
    agents: Sequence[str],
    release: bool,
) -> list[str]:
    """The exact `cargo run` invocation timed for one budget."""

    command = ["cargo", "run"]
    if release:
        command.append("--release")
    command += ["-p", "rosettad", "--", "serve"]
    command += ["--db", str(db_path), "--home", str(home)]
    if agents:
        for kind in agents:
            command += ["--agent", kind]
    else:
        command.append("--all-agents")
    command += ["--ticks", "0", "--slice-budget", budget]
    return command


def _signal_group(process: subprocess.Popen[str], number: int) -> None:
    try:
        os.killpg(os.getpgid(process.pid), number)
    except OSError:
        try:
            process.send_signal(number)
        except OSError:
            pass


def _stop(process: subprocess.Popen[str], expired: threading.Event | None = None) -> None:
    """End the run's whole process group — cargo's child daemon included."""

    if expired is not None:
        if process.poll() is not None:
            return  # the run finished as the watchdog fired; it did not time out
        expired.set()
    _signal_group(process, signal.SIGTERM)
    deadline = time.monotonic() + GRACE_SECONDS
    while time.monotonic() < deadline:
        if process.poll() is not None:
            return
        time.sleep(0.2)
    _signal_group(process, signal.SIGKILL)


def _describe_exit(code: int, expired: bool, timeout: float) -> tuple[bool, str]:
    if expired:
        return False, f"timed out after {timeout:g}s"
    if code == 0:
        return True, "exit 0"
    if code < 0:
        return False, f"killed by signal {-code}"
    return False, f"exit {code}"


def run_budget(
    budget: str,
    *,
    home: Path,
    agents: Sequence[str],
    repo_root: Path,
    timeout: float,
    release: bool,
    keep: bool,
    log_path: Path | None,
) -> RunResult:
    """Run serve once at one budget in a fresh projection directory."""

    work_dir = Path(tempfile.mkdtemp(prefix="rosetta-slice-sweep-"))
    command = build_command(
        budget,
        db_path=work_dir / "projection.sqlite",
        home=home,
        agents=agents,
        release=release,
    )
    milestones: dict[str, dict[str, float]] = {}
    tail: deque[str] = deque(maxlen=TAIL_LINES)
    expired = threading.Event()
    log = log_path.open("w", encoding="utf-8") if log_path is not None else None
    started = time.monotonic()
    try:
        process = subprocess.Popen(
            command,
            cwd=repo_root,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            bufsize=1,
            start_new_session=True,
        )
    except FileNotFoundError as error:
        if log is not None:
            log.close()
        raise SweepError(f"cannot run {command[0]}: {error}") from error
    watchdog = threading.Timer(timeout, _stop, args=(process, expired))
    watchdog.start()
    try:
        assert process.stdout is not None
        for line in process.stdout:
            if log is not None:
                log.write(line)
            tail.append(line.rstrip("\n"))
            parsed = parse_milestone_line(line)
            if parsed is not None:
                milestones.setdefault(parsed[0], parsed[1])
        code = process.wait()
    except BaseException:
        # Ctrl-C reaches this harness but not the child's own session.
        _stop(process)
        raise
    finally:
        watchdog.cancel()
        if process.stdout is not None:
            process.stdout.close()
        if log is not None:
            log.close()
        if not keep:
            shutil.rmtree(work_dir, ignore_errors=True)
    ok, status = _describe_exit(code, expired.is_set(), timeout)
    return RunResult(
        budget=budget,
        milestones=milestones,
        wall_s=time.monotonic() - started,
        ok=ok,
        status=status,
        tail=list(tail),
        log_path=log_path,
        work_dir=work_dir if keep else None,
    )


def prebuild(repo_root: Path, *, release: bool) -> None:
    """Build the daemon once so cargo's compile time is charged to no run."""

    command = ["cargo", "build", "-p", "rosettad"]
    if release:
        command.append("--release")
    try:
        completed = subprocess.run(command, cwd=repo_root, check=False)
    except FileNotFoundError as error:
        raise SweepError(f"cannot run {command[0]}: {error}") from error
    if completed.returncode != 0:
        raise SweepError(f"`{' '.join(command)}` failed with exit {completed.returncode}")


def resolve_repo_root() -> Path:
    root = Path(__file__).resolve().parents[2]
    if not (root / "Cargo.toml").is_file() or not (root / "crates" / "rosettad").is_dir():
        raise SweepError(f"{root} does not look like the rosetta repository root")
    return root


def resolve_home(value: str) -> Path:
    """Accept only a frozen snapshot directory — never the live home."""

    home = Path(value).expanduser().resolve()
    if not home.is_dir():
        raise SweepError(f"--home {value}: not a directory")
    if home == Path.home().resolve():
        raise SweepError(
            f"--home {home} is the live home directory; sweep only frozen, "
            "permission-read-only snapshots"
        )
    return home


def _log_name(budget: str) -> str:
    return "slice-sweep-" + re.sub(r"[^0-9A-Za-z]+", "-", budget).strip("-") + ".log"


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="sweep.py",
        description="Sweep rosettad serve starter-slice budgets over a frozen snapshot.",
    )
    parser.add_argument("--home", required=True, help="frozen agent-data snapshot")
    parser.add_argument(
        "--budgets",
        default=DEFAULT_BUDGETS,
        help=f"comma-separated --slice-budget values (default: {DEFAULT_BUDGETS})",
    )
    parser.add_argument(
        "--timeout",
        type=float,
        default=DEFAULT_TIMEOUT,
        metavar="SECS",
        help=f"per-run limit before the run is killed and failed (default: {DEFAULT_TIMEOUT:g})",
    )
    parser.add_argument("--out", metavar="results.csv", help="write the CSV here")
    parser.add_argument(
        "--keep",
        action="store_true",
        help="retain each run's temporary projection directory and write per-run logs",
    )
    parser.add_argument(
        "--debug-build", action="store_true", help="run the debug profile, not --release"
    )
    parser.add_argument(
        "--agent",
        action="append",
        default=[],
        metavar="KIND",
        dest="agents",
        help="restrict to one agent kind; repeatable, replaces --all-agents",
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    arguments = build_parser().parse_args(argv)
    out_path = Path(arguments.out).expanduser() if arguments.out else None
    try:
        budgets = parse_budgets(arguments.budgets)
        home = resolve_home(arguments.home)
        repo_root = resolve_repo_root()
        if arguments.timeout <= 0:
            raise SweepError("--timeout must be positive")
        if out_path is not None and not out_path.parent.is_dir():
            raise SweepError(f"--out {arguments.out}: {out_path.parent} is not a directory")
        release = not arguments.debug_build
        prebuild(repo_root, release=release)
    except SweepError as error:
        print(f"sweep: {error}", file=sys.stderr)
        return 2

    log_dir = out_path.parent if out_path is not None else Path.cwd()
    results: list[RunResult] = []
    for index, (budget, size) in enumerate(budgets, start=1):
        print(
            f"sweep: [{index}/{len(budgets)}] --slice-budget {budget} ({size} bytes)",
            file=sys.stderr,
        )
        try:
            results.append(
                run_budget(
                    budget,
                    home=home,
                    agents=arguments.agents,
                    repo_root=repo_root,
                    timeout=arguments.timeout,
                    release=release,
                    keep=arguments.keep,
                    log_path=log_dir / _log_name(budget) if arguments.keep else None,
                )
            )
        except SweepError as error:
            print(f"sweep: {error}", file=sys.stderr)
            return 2
        except KeyboardInterrupt:
            print("sweep: interrupted; the sweep is incomplete", file=sys.stderr)
            return 130
        print(f"sweep:   {results[-1].status}", file=sys.stderr)

    if out_path is not None:
        with out_path.open("w", encoding="utf-8", newline="") as handle:
            writer = csv.writer(handle)
            writer.writerow(COLUMNS)
            writer.writerows(csv_row(result) for result in results)
        print(f"sweep: wrote {out_path}", file=sys.stderr)

    print(format_table(results))

    for result in results:
        if result.ok:
            continue
        print(f"sweep: budget {result.budget} failed: {result.status}", file=sys.stderr)
        if result.log_path is not None:
            print(f"sweep:   full output in {result.log_path}", file=sys.stderr)
        for line in result.tail:
            print(f"sweep:   | {line}", file=sys.stderr)
    for result in results:
        if result.work_dir is not None:
            print(f"sweep: kept {result.work_dir}", file=sys.stderr)

    return 0 if any(result.ok for result in results) else 1


if __name__ == "__main__":
    raise SystemExit(main())
