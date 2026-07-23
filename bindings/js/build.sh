#!/usr/bin/env bash
# Build the npm package into bindings/js/pkg/ and fix its metadata.
# Publish with: cd pkg && npm publish --access public
set -euo pipefail
cd "$(dirname "$0")"

# opt-level=z only for this build (the workspace release profile stays fast
# for the CLI); applied via cargo config override.
wasm-pack build --target web --release --out-dir pkg -- --config 'profile.release.opt-level="z"'

# npm auto-includes only files named LICENSE/LICENCE (with optional dot
# extension) — "LICENSE-MIT" is skipped, so ship a plain LICENSE copy.
cp LICENSE-MIT pkg/LICENSE

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
// Belt and braces: npm auto-includes LICENSE, but list it explicitly too.
if (Array.isArray(p.files) && !p.files.includes('LICENSE')) p.files.push('LICENSE');
fs.writeFileSync('pkg/package.json', JSON.stringify(p, null, 2) + '\n');
console.log(`pkg ready: ${p.name}@${p.version}`);
EOF
