#!/usr/bin/env bash
set -euo pipefail

target=${1:?usage: package-release.sh TARGET}
host=$(rustc -vV | sed -n 's/^host: //p')
if [[ "$target" != "$host" ]]; then
  echo "Native release runner required: requested $target, got $host" >&2
  exit 1
fi
case "$target" in
  x86_64-unknown-linux-gnu|aarch64-unknown-linux-gnu|aarch64-apple-darwin|x86_64-apple-darwin) ;;
  *) echo "Unsupported release target: $target" >&2; exit 1 ;;
esac

version=$(python3 -c 'import tomllib; print(tomllib.load(open("Cargo.toml", "rb"))["workspace"]["package"]["version"])')
bundle="rosettad-$version-$target"
staging=$(mktemp -d)
trap 'rm -rf "$staging"' EXIT

cargo build --release --locked --package rosettad --target "$target"
target_dir=$(cargo metadata --format-version 1 --no-deps --locked | python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')
binary="$target_dir/$target/release/rosettad"
mkdir -p "$staging/empty-home" "$staging/$bundle" dist
"$binary" discover --home "$staging/empty-home"
cp "$binary" "$staging/$bundle/rosettad"
cp README.md "$staging/$bundle/README.md"
python3 - "$version" "$target" "$staging/$bundle/BUILD.json" <<'PY'
import json, pathlib, subprocess, sys
version, target, path = sys.argv[1:]
metadata = {
    "version": version,
    "target": target,
    "commit": subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip(),
    "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
}
pathlib.Path(path).write_text(json.dumps(metadata, indent=2) + "\n")
PY
tar -czf "dist/$bundle.tar.gz" -C "$staging" "$bundle"
(
  cd dist
  shasum -a 256 "$bundle.tar.gz" > "$bundle.tar.gz.sha256"
  shasum -a 256 --check "$bundle.tar.gz.sha256"
)
