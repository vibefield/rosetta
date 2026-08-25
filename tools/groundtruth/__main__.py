"""Command dispatcher for `python3 -m tools.groundtruth`."""

from __future__ import annotations

import sys

from . import census, check, farm, mock_model, render

COMMANDS = {
    "census": census.main,
    "check": check.main,
    "farm": farm.main,
    "mock-model": mock_model.main,
    "render": render.main,
}


def main(argv: list[str] | None = None) -> int:
    arguments = list(sys.argv[1:] if argv is None else argv)
    if not arguments or arguments[0] in {"-h", "--help"}:
        print("usage: python3 -m tools.groundtruth <command> [args]\n")
        print("commands:")
        print("  census     capture a privacy-bounded organic structure census")
        print("  check      validate layouts, canonical JSON, fixtures, and HTML")
        print("  farm       generate fixtures and rebuild canonical artifacts")
        print("  mock-model run the deterministic local model API")
        print("  render     render canonical JSON as self-contained HTML")
        return 0
    command = arguments.pop(0)
    handler = COMMANDS.get(command)
    if handler is None:
        print(f"unknown command: {command}", file=sys.stderr)
        return 2
    return handler(arguments)


if __name__ == "__main__":
    raise SystemExit(main())
