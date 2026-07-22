#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

if ! cargo cyclonedx --version >/dev/null 2>&1; then
    echo "cargo-cyclonedx is required; install it with: cargo install cargo-cyclonedx --locked" >&2
    exit 1
fi

cargo cyclonedx \
    --format json \
    --features native-backend \
    --override-filename sbom.cdx

test -s sbom.cdx.json

if command -v jq >/dev/null 2>&1; then
    jq empty sbom.cdx.json
else
    python3 -m json.tool sbom.cdx.json >/dev/null
fi

echo "generated $repo_root/sbom.cdx.json"
