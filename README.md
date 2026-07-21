# dwg2geo

Starter repository for an open-source CLI that converts engineering DWG drawings to GeoJSON with explicit coordinate-reference handling, diagnostics, and traceable conversion reports.

## Current scope

The starter implements a conservative external-tool MVP:

- `dwg2geo inspect`: reads the six-byte DWG signature, reports the AutoCAD generation, size, and SHA-256 without requiring LibreDWG or GDAL.
- `dwg2geo doctor`: checks whether `dwgread` and `ogr2ogr` are available.
- `dwg2geo convert`: either:
  - converts DWG -> DXF with LibreDWG and reprojects DXF -> GeoJSON with GDAL, when `--source-crs` is provided; or
  - exports raw/local coordinates directly through LibreDWG only when `--allow-local-coordinates` is explicit.
- `--backend native` is reserved for the `acadrust` implementation described in the roadmap.

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
