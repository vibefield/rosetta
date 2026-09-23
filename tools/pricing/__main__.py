from __future__ import annotations

import argparse
import sys
from pathlib import Path

from .generate import GENERATED, PricingError, build, write


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="python3 -m tools.pricing",
        description="Generate crates/pricing/src/generated.rs from the vendored catalog.",
    )
    commands = parser.add_subparsers(dest="command", required=True)
    generate = commands.add_parser("generate", help="write the generated Rust catalog")
    generate.add_argument(
        "--check",
        action="store_true",
        help="fail if the committed generated catalog is stale instead of writing it",
    )
    args = parser.parse_args(argv)
    try:
        source = build()
        if args.command == "generate":
            if args.check:
                current = GENERATED.read_text() if GENERATED.exists() else ""
                if current != source:
                    print(
                        f"{GENERATED} is stale; run `python3 -m tools.pricing generate`",
                        file=sys.stderr,
                    )
                    return 1
                print(f"{GENERATED} is current")
                return 0
            write(source)
            print(f"wrote {GENERATED.relative_to(Path.cwd()) if GENERATED.is_relative_to(Path.cwd()) else GENERATED}")
    except PricingError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
