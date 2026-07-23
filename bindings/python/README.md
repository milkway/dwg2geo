# dwg2geo (Python)

Convert engineering **DWG drawings to GeoJSON** from Python — the pure-Rust [dwg2geo](https://github.com/milkway/dwg2geo) converter as a native extension. No AutoCAD, no LibreDWG, no GDAL needed.

```bash
pip install dwg2geo
```

```python
import json
import dwg2geo

result = dwg2geo.convert_file("drawing.dwg")           # or dwg2geo.convert(raw_bytes)
fc = json.loads(result["geojson"])                     # GeoJSON FeatureCollection (local drawing coords)

print(result["feature_count"], "features")
print(result["converted"])                             # per-entity-type counts
print(result["skipped"], result["failed"])             # with reasons — auditable
```

Options: `convert(data, polygonize_closed=False, curve_tolerance=None)`.

The result dict mirrors dwg2geo's `EmbedResult`:

| key | meaning |
|---|---|
| `geojson` | `FeatureCollection` string in the drawing's local coordinates |
| `feature_count`, `model_space_entities` | totals |
| `converted` / `skipped` / `failed` | per-entity-type outcomes with reasons |
| `warnings` | conversion warnings |
| `bbox` | `[minx, miny, maxx, maxy]` or `None` |
| `source_sha256` | hash of the input bytes (audit trail) |

Features carry resolved CAD style metadata (`layer`, `color_rgb`, `color_index`, `linetype`, `lineweight_mm`, text properties…). Coordinates are **local** — georeference with e.g. [pyproj](https://pyproj4.github.io/pyproj/) using the drawing's known CRS; dwg2geo never guesses one:

```python
from pyproj import Transformer
transformer = Transformer.from_crs("EPSG:31983", "EPSG:4326", always_xy=True)
lon, lat = transformer.transform(x, y)
```

Deterministic: the same bytes always produce byte-identical GeoJSON on a given platform (across platforms — native vs WebAssembly — a few floating-point values may differ in the last digit).

License: MIT.
