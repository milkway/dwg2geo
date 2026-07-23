#!/usr/bin/env bash
# Build the npm package into bindings/js/pkg/ and fix its metadata.
# Publish with: cd pkg && npm publish --access public
set -euo pipefail
cd "$(dirname "$0")"

# opt-level=z only for this build (the workspace release profile stays fast
# for the CLI); applied via cargo config override.
wasm-pack build --target web --release --out-dir pkg -- --config 'profile.release.opt-level="z"'

# wasm-pack names the package after the crate (dwg2geo-wasm); the npm package
# is published as plain `dwg2geo`, with the repo metadata npm expects.
node - <<'EOF'
const fs = require('fs');
const p = JSON.parse(fs.readFileSync('pkg/package.json', 'utf8'));
p.name = 'dwg2geo';
p.repository = { type: 'git', url: 'git+https://github.com/milkway/dwg2geo.git', directory: 'bindings/js' };
p.homepage = 'https://github.com/milkway/dwg2geo';
p.keywords = ['dwg', 'geojson', 'cad', 'gis', 'wasm', 'webassembly', 'autocad', 'converter'];
p.license = 'MIT';
fs.writeFileSync('pkg/package.json', JSON.stringify(p, null, 2) + '\n');
console.log(`pkg ready: ${p.name}@${p.version}`);
EOF
