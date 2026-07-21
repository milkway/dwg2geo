# Implementation plan

## Success criteria

The project succeeds when it can convert representative engineering DWGs into GeoJSON while producing enough evidence to answer:

- what was converted;
- what was approximated;
- what was skipped and why;
- which CRS and units were used;
- how blocks and coordinate systems were transformed;
- whether the output passes geometry and count validation.

## Milestone 0 — repository bootstrap

- [x] Create Rust CLI structure.
- [x] Add `inspect`, `doctor`, and `convert` command surfaces.
- [x] Add a conservative external LibreDWG/GDAL pipeline.
- [x] Record reference-file metadata without including the proprietary DWG.
- [x] Add agent prompts, architecture, risks, and entity mapping.
- [ ] Run formatting, compilation, Clippy, and tests on a Rust-enabled machine.
- [ ] Initialize git and create the first commit.

Exit condition: the starter compiles and its unit tests pass.

## Milestone 1 — external backend MVP

- [ ] Add integration tests for CLI validation and exit codes.
- [ ] Add structured `doctor --json` output.
- [ ] Detect LibreDWG and GDAL versions and include them in conversion reports.
- [ ] Write a sidecar `<output>.report.json` containing options, tools, source hash, timings, and warnings.
- [ ] Add `--keep-intermediate` for diagnostic DXF retention.
- [ ] Add layer include/exclude options to the GDAL route where supported.
- [ ] Ensure paths containing spaces and non-ASCII characters work.
- [ ] Add explicit overwrite and partial-output cleanup behavior.
- [ ] Test a local AC1027 drawing manually and record the observed entity counts outside git if proprietary.

Exit condition: a user with LibreDWG and GDAL can perform a traceable, CRS-explicit conversion.

## Milestone 2 — native inspection with acadrust

- [ ] Enable and compile the `native-backend` Cargo feature.
- [ ] Read AC1027 into `CadDocument` using `acadrust`.
- [ ] Implement `layers` with counts by entity type and model/paper space.
- [ ] Extend `inspect` with drawing units, extents, layer count, block count, and entity histogram.
- [ ] Detect unsupported/corrupt objects without crashing the whole report when recoverable.
- [ ] Add synthetic or redistributable DWG fixtures for supported versions.
- [ ] Compare native inspection counts against LibreDWG output on the local reference file.

Exit condition: native inspection is stable enough to drive conversion planning and detect feature loss.

## Milestone 3 — native 2D geometry conversion

Implement entities in this order:

- [ ] `POINT` -> `Point`.
- [ ] `LINE` -> `LineString`.
- [ ] `LWPOLYLINE` without bulges -> `LineString` / optional `Polygon`.
- [ ] Classic `POLYLINE` / `VERTEX`.
- [ ] Bulge arc tessellation with deterministic tolerance.
- [ ] `ARC` and `CIRCLE` tessellation.
- [ ] `ELLIPSE` tessellation.
- [ ] `SPLINE` evaluation/tessellation.
- [ ] `TEXT` and `MTEXT` as point features with text properties.
- [ ] `3DFACE` projected to configured XY behavior.
- [ ] `HATCH` boundary extraction with holes and ring repair diagnostics.

Cross-cutting tasks:

- [ ] OCS -> WCS conversion.
- [ ] model-space filtering by default;
- [ ] stable feature IDs;
- [ ] per-entity error isolation;
- [ ] geometry-validity checks;
- [ ] `GeoJSONSeq` streaming mode for large drawings.

Exit condition: common civil/utility plan geometry converts natively with quantified losses.

## Milestone 4 — blocks and references

- [ ] Read block definitions and `INSERT` references.
- [ ] Compose translation, rotation, non-uniform scale, extrusion, and nested transforms.
- [ ] Detect recursive block references.
- [ ] Add `--explode-blocks` and `--preserve-inserts` modes.
- [ ] Preserve block path and attributes in feature properties.
- [ ] Resolve BYLAYER/BYBLOCK metadata where relevant.

Exit condition: nested engineering symbols and repeated structures are spatially correct and traceable.

## Milestone 5 — CRS, units, and calibration

- [ ] Read drawing units and require an override when absent or ambiguous.
- [ ] Reproject using the Rust `proj` crate.
- [ ] Support local affine calibration from at least two control points.
- [ ] Add residual/error reporting for three or more control points.
- [ ] Reject implausible EPSG:4326 extents unless explicitly overridden.
- [ ] Record axis order, units, source CRS, target CRS, and transformation pipeline.

Exit condition: output positioning is explicit, reproducible, and sanity checked.

## Milestone 6 — validation and quality gates

- [ ] Compare source entity histogram against converted/skipped histogram.
- [ ] Report bounding boxes before and after transformation.
- [ ] Detect NaN, infinite, duplicate, empty, and degenerate geometries.
- [ ] Validate polygon ring closure and orientation.
- [ ] Add golden GeoJSON tests with coordinate tolerances.
- [ ] Add property-based tests for arc tessellation and affine transforms.
- [ ] Differentially compare native output with LibreDWG/GDAL on a fixture corpus.
- [ ] Establish acceptable loss thresholds per entity class.

Exit condition: releases have measurable correctness criteria rather than visual confidence alone.

## Milestone 7 — distribution

- [ ] Linux, macOS, and Windows CI.
- [ ] Release binaries and checksums.
- [ ] Shell completion and man page.
- [ ] Container image with external tools as an optional distribution.
- [ ] SBOM and dependency-license report.
- [ ] Document external LibreDWG/GDAL licensing and distribution boundaries.

Exit condition: users can install a repeatable build and understand its dependencies and limitations.

## First terminal session

1. Install Rust stable.
2. Extract this package and enter the directory.
3. Run `cargo fmt --check`, `cargo check`, and `cargo test`.
4. Fix any API/version drift without changing product behavior.
5. Complete the remaining Milestone 0 checkboxes.
6. Start Milestone 1 in order.
