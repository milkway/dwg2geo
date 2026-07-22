#!/bin/sh
# Fetch the georeferenced municipal boundary of Sorocaba/SP (IBGE code
# 3552205) from the IBGE "malhas" API as WGS 84 GeoJSON, for CRS validation
# and control-point calibration of the local reference drawings.
#
# Output: samples/sorocaba-limite-municipal.geojson (git-ignored local data;
# rerun this script to refresh). Pass a different IBGE municipality code and
# output path to fetch another boundary.
set -eu

CODE="${1:-3552205}"
OUT="${2:-samples/sorocaba-limite-municipal.geojson}"

curl -fsS \
  -H "Accept: application/vnd.geo+json" \
  "https://servicodados.ibge.gov.br/api/v3/malhas/municipios/${CODE}?formato=application/vnd.geo%2Bjson&qualidade=intermediaria" \
  -o "${OUT}"

python3 - "$OUT" <<'EOF'
import json, sys
d = json.load(open(sys.argv[1]))
kind = d.get("type")
count = len(d.get("features", [])) if kind == "FeatureCollection" else 1
print(f"wrote {sys.argv[1]}: {kind}, {count} feature(s)")
EOF
