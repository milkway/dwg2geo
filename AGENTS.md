# AGENTS.md — implementation contract

You are working on `dwg2geo`, a Rust CLI for converting engineering DWG files to GeoJSON.

## Read first

Before modifying code, read:

1. `README.md`
2. `docs/PLAN.md`
3. `docs/ARCHITECTURE.md`
4. `docs/ENTITY_MAPPING.md`
5. `docs/DECISIONS.md`
6. `docs/RISKS.md`

## Product objective

Create a dependable, auditable CLI that:

- reads common engineering DWG files, initially AC1027 / AutoCAD 2013 generation;
- maps supported CAD entities into GeoJSON geometries and properties;
- handles blocks, coordinate systems, curves, units, layers, and model space explicitly;
- reports every unsupported, skipped, approximated, or repaired entity;
- supports both an external-tool backend and, later, a native Rust backend;
- never presents unreferenced CAD coordinates as geographically correct.

## Non-negotiable rules

1. **Fail closed on CRS.** Require a source CRS or an explicit local-coordinate override.
2. **No silent loss.** Every unsupported or failed entity increments diagnostics and appears in the report.
3. **No DWG parser from scratch.** Use `acadrust` for the native backend and LibreDWG for the external backend.
4. **Deterministic output.** Given the same file and options, feature order, identifiers, reports, and curve approximation must be stable.
5. **Provenance.** Preserve at least layer, entity type, handle when available, block path, source file, and conversion warnings.
6. **Engineering geometry first.** Implement OCS/WCS transforms, INSERT transforms, bulge arcs, closed-polyline semantics, and curve tolerance before broad entity-count claims.
7. **No proprietary fixtures in git.** Tests must use synthetic or redistributable fixtures. The local sample is ignored by git.
8. **Small verified increments.** Complete one roadmap slice with tests and documentation before broad refactors.

## Work protocol

At the beginning of a session:

```bash
git status --short
cargo fmt --check
cargo check
cargo test
```

If the starter does not compile, fix compilation and tests before implementing new features. State what was broken in the commit message or session summary.

When implementing a roadmap item:

- write or update tests first where practical;
- add diagnostics for failure paths;
- update the relevant document and checkbox in `docs/PLAN.md`;
- run `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, and `cargo test`;
- show a concise summary of changed files, commands run, and remaining risks.

## Current priority

Stabilize **Milestone 1 — external backend MVP**:

- verify that the starter builds on the installed stable Rust toolchain;
- test DWG signature inspection with synthetic files;
- test command validation without requiring LibreDWG or GDAL;
- make `doctor` output useful and machine-readable later;
- ensure failed subprocesses include actionable stderr;
- keep raw/local-coordinate export opt-in.

After Milestone 1 is complete, proceed to **Milestone 2 — native inspection** using the optional `native-backend` feature and `acadrust`.

## Native backend boundaries

Do not let `acadrust` types leak through the entire program. Convert them into an internal CAD-neutral model containing:

- source entity identity;
- layer and style metadata;
- model/paper-space membership;
- geometry in drawing/world coordinates;
- block expansion provenance;
- warning and approximation metadata.

The GeoJSON writer consumes only this internal model.

## Definition of done for a conversion feature

A feature is done only when:

- happy-path and failure-path tests exist;
- unsupported cases are reported, not ignored;
- CLI help is updated;
- output is deterministic;
- CRS and units behavior is explicit;
- the sample-file validation checklist is updated without committing the file.

## Commands to preserve

```text
dwg2geo doctor
dwg2geo inspect <FILE> [--json]
dwg2geo convert <FILE> --output <FILE> --source-crs <CRS> [--target-crs EPSG:4326]
dwg2geo convert <FILE> --output <FILE> --allow-local-coordinates
```

Future commands may include:

```text
dwg2geo layers <FILE>
dwg2geo entities <FILE>
dwg2geo validate <GEOJSON> --against <REPORT>
dwg2geo calibrate <FILE> --control-point ...
```

Do not break existing command syntax without a migration note.
