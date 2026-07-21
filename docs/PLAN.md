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
- [x] Run formatting, compilation, Clippy, and tests on a Rust-enabled machine.
- [x] Initialize git and create the first commit.

Exit condition: the starter compiles and its unit tests pass.

## Milestone 1 — external backend MVP

- [x] Add integration tests for CLI validation and exit codes.
- [x] Add structured `doctor --json` output.
- [x] Detect LibreDWG and GDAL versions and include them in conversion reports.
- [x] Write a sidecar `<output>.report.json` containing options, tools, source hash, timings, and warnings.
- [x] Add `--keep-intermediate` for diagnostic DXF retention.
- [x] Add layer include/exclude options to the GDAL route where supported.
- [x] Ensure paths containing spaces and non-ASCII characters work.
- [x] Add explicit overwrite and partial-output cleanup behavior.
- [x] Test a local AC1027 drawing manually and record the observed entity counts outside git if proprietary. (Recorded in `samples/validation-corredor-sul.local.md`, git-ignored. Found and fixed: LibreDWG's direct GeoJSON route emits bare `-nan` for NaN coordinates; conversions now validate JSON well-formedness before delivering output.)

Exit condition: a user with LibreDWG and GDAL can perform a traceable, CRS-explicit conversion.

## Milestone 2 — native inspection with acadrust

- [x] Enable and compile the `native-backend` Cargo feature. (PROJ reprojection moved to a separate `native-reproject` feature; see ADR-009.)
- [x] Read AC1027 into `CadDocument` using `acadrust`.
- [x] Implement `layers` with counts by entity type and model/paper space.
- [x] Extend `inspect` with drawing units, extents, layer count, block count, and entity histogram.
- [x] Detect unsupported/corrupt objects without crashing the whole report when recoverable. (Strict parse first, failsafe re-read on failure; empty failsafe recoveries are rejected, unknown entities and unresolved handles are counted.)
- [x] Add synthetic or redistributable DWG fixtures for supported versions. (Fixtures are generated at test time with the `acadrust` DWG writer, so no binary files enter git.)
- [x] Compare native inspection counts against LibreDWG output on the local reference file. (8/17 entity types match exactly; native counts 219 more entities (~2.5%) than LibreDWG's DXF export, one-directional, with 5 block definitions missing from the DXF. Adjudication deferred to Milestone 6. Aggregates in the git-ignored local validation note.)

Exit condition: native inspection is stable enough to drive conversion planning and detect feature loss.

## Milestone 3 — native 2D geometry conversion

Implement entities in this order:

- [x] `POINT` -> `Point`.
- [x] `LINE` -> `LineString`.
- [x] `LWPOLYLINE` without bulges -> `LineString` / optional `Polygon`. (`--polygonize-closed`; closed polylines stay LineStrings by default per ADR-006. Bulged polylines are skipped with an explicit reason until bulge tessellation lands.)
- [ ] Classic `POLYLINE` / `VERTEX`.
- [ ] Bulge arc tessellation with deterministic tolerance.
- [ ] `ARC` and `CIRCLE` tessellation.
- [ ] `ELLIPSE` tessellation.
- [ ] `SPLINE` evaluation/tessellation.
- [ ] `TEXT` and `MTEXT` as point features with text properties.
- [ ] `3DFACE` projected to configured XY behavior.
- [ ] `HATCH` boundary extraction with holes and ring repair diagnostics.

Cross-cutting tasks:

- [x] OCS -> WCS conversion. (Arbitrary axis algorithm applied to the entity types converted so far; each new curved type must reuse it.)
- [x] model-space filtering by default; paper-space, block-definition, and unowned entities are counted as excluded in the report.
- [x] stable feature IDs; entity handles, with a document-order fallback for null handles.
- [x] per-entity error isolation; failures and skips are per-entity outcomes with reasons and sample handles, never command aborts.
- [x] geometry-validity checks — non-finite coordinates, degenerate lines, sub-minimal rings. (Deeper checks — self-intersection, duplicate vertices — belong to Milestone 6.)
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
