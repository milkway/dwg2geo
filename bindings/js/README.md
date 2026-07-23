# dwg2geo (npm)

Convert engineering **DWG drawings to GeoJSON** entirely in the browser or Node — the pure-Rust [dwg2geo](https://github.com/milkway/dwg2geo) converter compiled to WebAssembly. No native dependencies, no server.

```bash
npm install dwg2geo
```

**Browser** (or any runtime where `fetch` resolves module-relative URLs):

```js
import init, { convert } from 'dwg2geo';

await init(); // fetches and instantiates the wasm module

const bytes = new Uint8Array(await file.arrayBuffer()); // a .dwg file
const result = convert(bytes, /* polygonize_closed */ false, /* curve_tolerance */ undefined);

const fc = JSON.parse(result.geojson); // GeoJSON FeatureCollection (local drawing coordinates)
console.log(result.feature_count, result.converted, result.skipped, result.warnings);
```

**Node** (no `fetch` for `file:` URLs — pass the wasm bytes explicitly):

```js
import { readFile } from 'node:fs/promises';
import init, { convert } from 'dwg2geo';

const wasm = await readFile(new URL('./node_modules/dwg2geo/dwg2geo_wasm_bg.wasm', import.meta.url));
await init({ module_or_path: wasm });

const result = convert(new Uint8Array(await readFile('drawing.dwg')), false, undefined);
```

The result mirrors dwg2geo's `EmbedResult`:

| field | meaning |
|---|---|
| `geojson` | `FeatureCollection` string in the drawing's local coordinates |
| `feature_count`, `model_space_entities` | totals |
| `converted` / `skipped` / `failed` | per-entity-type outcome counts with reasons |
| `warnings` | conversion warnings |
| `bbox` | `[minx, miny, maxx, maxy]` or `null` |
| `source_sha256` | hash of the input bytes (audit trail) |

Features carry resolved CAD style metadata (`layer`, `color_rgb`, `color_index`, `linetype`, `lineweight_mm`, text properties…). Coordinates are **local** — georeference them yourself (e.g. [proj4js](http://proj4js.org/)) with the drawing's known CRS; dwg2geo never guesses one.

Deterministic: the same bytes always produce byte-identical GeoJSON on a given platform (across platforms — native vs WebAssembly — a few floating-point values may differ in the last digit).

Built with `wasm-pack --target web`. See a full working app at [dwg2geo-app](https://github.com/milkway/dwg2geo-app) ([live demo](https://milkway.github.io/dwg2geo-app/)).

License: MIT.
