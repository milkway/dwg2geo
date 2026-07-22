# dwg2geo

Starter repository for an open-source CLI that converts engineering DWG drawings to GeoJSON with explicit coordinate-reference handling, diagnostics, and traceable conversion reports.

## Current scope

The starter implements a conservative external-tool MVP:

- `dwg2geo inspect`: reads the six-byte DWG signature, reports the AutoCAD generation, size, and SHA-256 without requiring LibreDWG or GDAL.
- `dwg2geo doctor`: checks whether `dwgread` and `ogr2ogr` are available, including versions; `--json` emits a machine-readable report and the exit code reflects tool health.
- `dwg2geo convert`: either:
  - converts DWG -> DXF with LibreDWG and reprojects DXF -> GeoJSON with GDAL, when `--source-crs` is provided; or
  - exports raw/local coordinates directly through LibreDWG only when `--allow-local-coordinates` is explicit.
- `--backend native` conversion is reserved for the roadmap, but native *inspection* is available today behind the `native-backend` feature (see below).

Every successful conversion also writes a sidecar report at `<output>.report.json` recording the CLI options, external tool versions, source signature and SHA-256, executed commands with timings, and warnings. Apart from the duration fields, the report is deterministic for the same input and options.

Convert options for traceability and control:

- `--keep-intermediate` keeps the LibreDWG DXF at `<output>.intermediate.dxf` for diagnostics (GDAL route).
- `--include-layers` / `--exclude-layers` restrict the GDAL route to a comma-separated layer subset via an attribute filter; they require `--source-crs`.
- Overwrites are explicit: an existing output fails without `--force`, and even with `--force` the previous file is replaced only after the new output is complete. Failed runs remove their partial output; nothing is silently truncated.

The uploaded reference drawing is **not included** in this repository. Its observed metadata is stored in `samples/corredor-sul.metadata.json`.

## Why fail closed on CRS?

A DWG can use SIRGAS 2000 / UTM, SAD69, a local engineering grid, millimetres, or arbitrary coordinates. GeoJSON output that merely copies CAD coordinates can be syntactically valid while geographically wrong. Therefore, conversion requires `--source-crs` unless the caller explicitly accepts local coordinates.

## Requirements

For repository development:

- Rust stable, edition 2024, Rust 1.85 or newer.

For the external conversion backend:

- GNU LibreDWG command `dwgread`.
- GDAL command `ogr2ogr` when reprojection is requested.

Package names vary by operating system. Confirm that these commands work:

```bash

dwgread --version
ogr2ogr --version
```

## Start

```bash
cargo check
cargo test
cargo run -- doctor
```

Copy the engineering file locally, without committing it:

```bash
mkdir -p samples
cp "/path/to/_Corredor Sul.dwg" samples/
```

Inspect it:

```bash
cargo run -- inspect "samples/_Corredor Sul.dwg" --json
```

Convert with a known source CRS:

```bash
cargo run -- convert \
  "samples/_Corredor Sul.dwg" \
  --output output/corredor-sul.geojson \
  --source-crs EPSG:31985 \
  --target-crs EPSG:4326
```

`EPSG:31985` above is only an example. Do not use it until the project's actual CRS has been confirmed.

Export local drawing coordinates only:

```bash
cargo run -- convert \
  "samples/_Corredor Sul.dwg" \
  --output output/corredor-sul-local.geojson \
  --allow-local-coordinates
```

## Native inspection (optional feature)

Building with the pure-Rust `acadrust` reader enables entity-level inspection without LibreDWG or GDAL:

```bash
cargo build --features native-backend
```

With the feature enabled:

- `inspect` additionally reports the DWG version, measurement system, insertion units, model extents, layer and block counts, and an entity histogram split by model space, paper space, block definitions, and unowned entities. A parse failure never hides the file-level report; it appears as an explicit `native_error`.
- `dwg2geo layers <FILE> [--json]` lists every layer with its flags and entity counts by type and space, plus any layer names referenced by entities but missing from the layer table.
- Corrupt files are retried in failsafe mode; recovered reports are labeled `failsafe_recovery` and carry the strict-parse error. Unknown entities and unresolved handles are counted, never dropped.
- `convert --backend native --allow-local-coordinates` converts model-space `POINT`, `LINE`, `LWPOLYLINE`, classic `POLYLINE` (2D and 3D), `ARC`, `CIRCLE`, `ELLIPSE`, `SPLINE`, `3DFACE`, and `TEXT`/`MTEXT` (as point features with text properties) to GeoJSON in raw drawing coordinates, entirely in-process. Bulge arc segments are tessellated deterministically under `--curve-tolerance` (max chord error in drawing units, default 0.05, with a 15° angular cap per segment); approximated features are flagged and counted. Closed polylines become closed LineStrings, or Polygons with CCW rings when `--polygonize-closed` is passed. Every skipped or failed entity appears in the report's `native` section with a reason and sample handles; the output carries a `dwg2geo` foreign member marking it as non-geographic. Reprojection (`--source-crs`) on the native backend is rejected until Milestone 5 (`native-reproject`).
- `INSERT` block references are expanded into their block geometry by default (`--explode-blocks` documents the choice): translation, insert-normal orientation, rotation, non-uniform scale, MINSERT row/column grids, and nesting up to 16 levels are composed per instance, with recursive or missing block definitions reported as failed INSERTs. Expanded features carry a `block_path` property and instance-unique ids; block content on layer `0` inherits the insert's layer (the original is kept in `source_layer`). `--preserve-inserts` instead emits each INSERT as a point feature with `block_name`, rotation, and attribute values; inserts with attributes also emit that anchor point when exploding. Every feature carries resolved style metadata: ByLayer/ByBlock colors and linetypes are resolved through the layer table and the insert chain into `color_index`/`color_rgb`/`linetype` (unresolvable policies are emitted verbatim), and text rotation inside blocks follows the insert's rotation.

The separate `native-reproject` feature adds the `proj` crate for Milestone 5 reprojection; it needs system PROJ >= 9.6 or a build toolchain with cmake and sqlite3 (see `docs/DECISIONS.md`, ADR-009).

## Continue with an AI coding agent

Read one of these prompts into the terminal agent:

```bash
codex "$(cat prompts/START_CODEX.md)"
```

or:

```bash
claude "$(cat prompts/START_CLAUDE.md)"
```

The canonical engineering instructions are in `AGENTS.md`. Claude-specific guidance is in `CLAUDE.md`.

## Repository map

```text
src/                    Rust CLI starter
samples/                local-only drawings and reference metadata
docs/PLAN.md            phased implementation plan
docs/ARCHITECTURE.md    target architecture and boundaries
docs/ENTITY_MAPPING.md  CAD-to-GeoJSON mapping rules
docs/DECISIONS.md       initial architectural decisions
docs/RISKS.md           engineering and delivery risks
prompts/                 ready-to-paste agent prompts
```

## Important constraints

- Never claim geographic correctness unless the source CRS or a control-point transformation is known.
- Never silently discard unsupported entities.
- Preserve CAD provenance in feature properties and in a conversion report.
- Do not commit proprietary or client DWG files.
- Do not implement the DWG binary format from scratch.
