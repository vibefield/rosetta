"""Reject malformed tags and tags that do not match the workspace version."""

import re
import sys
import tomllib

with open("Cargo.toml", "rb") as manifest:
    version = tomllib.load(manifest)["workspace"]["package"]["version"]

tag = sys.argv[1]
if not re.fullmatch(r"v(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)", tag):
    raise SystemExit("Release tags must be stable versions such as v0.1.0")
if tag != f"v{version}":
    raise SystemExit(f"Tag {tag} does not match workspace version {version}")
print(f"Validated {tag}")
