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
- [x] Classic `POLYLINE` / `VERTEX`. (2D and 3D variants; curve-fit/spline-fit smoothing and meshes are skipped with explicit reasons until implemented.)
- [x] Bulge arc tessellation with deterministic tolerance. (`--curve-tolerance`, default 0.05 drawing units; 15°/segment angular cap and 256-segment arc cap with a warning; applies to LWPOLYLINE and 2D POLYLINE segments including the closing segment.)
- [x] `ARC` and `CIRCLE` tessellation. (Shared arc tessellator with the bulge path: same chord tolerance, angular cap, and segment cap. Circles close their ring and honor `--polygonize-closed`; arcs sweep CCW in the OCS plane; features are marked approximated.)
- [x] `ELLIPSE` tessellation. (Parametric evaluation in WCS; the circle step formula with the major radius bounds the chord error. Full ellipses close their ring and honor `--polygonize-closed`.)
- [x] `SPLINE` evaluation/tessellation. (De Boor on homogeneous coordinates, rational weights supported; uniform parameter sampling at 8 segments/span within [16, 256] — chord tolerance is not applied to splines yet. Invalid NURBS data falls back to a polyline through fit points with a warning, or an explicit skip.)
- [x] `TEXT` and `MTEXT` as point features with text properties. (Anchor point, value, height, rotation in degrees, style. TEXT anchors are lifted from OCS; MTEXT inline format codes are stripped into `text` with the raw value kept in `text_raw` when different.)
- [x] `3DFACE` projected to configured XY behavior. (WCS corners projected through INSERT placement to an always-Polygon CCW ring; z is dropped with a warning, duplicate triangle corners are collapsed, and degenerate faces are skipped.)
- [x] `SOLID` filled quads/triangles -> `Polygon`. (OCS corners lifted through the arbitrary-axis transform; the DXF bow-tie corner order 1-2-4-3 is untwisted, duplicate triangle corners collapse, degenerate solids are skipped. TRACE is not exposed by acadrust 0.4.1.)
- [x] `HATCH` boundary extraction with holes and ring repair diagnostics. (Polyline paths reuse the bulge tessellator; edge paths chain line/arc/elliptic-arc/spline edges, reversing edges whose far end connects better. Gaps within the chord tolerance snap silently; larger gaps are bridged/closed with repair warnings and mark the feature approximated. Loops nest by even-odd containment into Polygon/MultiPolygon with CCW shells and CW holes; invalid loops are dropped with a count in `hatch_loops_dropped`, and a hatch with no valid loops is skipped with a reason.)

Cross-cutting tasks:

- [x] OCS -> WCS conversion. (Arbitrary axis algorithm applied to the entity types converted so far; each new curved type must reuse it.)
- [x] model-space filtering by default; paper-space, block-definition, and unowned entities are counted as excluded in the report.
- [x] stable feature IDs; entity handles, with a document-order fallback for null handles.
- [x] per-entity error isolation; failures and skips are per-entity outcomes with reasons and sample handles, never command aborts.
- [x] geometry-validity checks — non-finite coordinates, degenerate lines, sub-minimal rings. (Deeper checks — self-intersection, duplicate vertices — belong to Milestone 6.)
- [x] `GeoJSONSeq` streaming mode for large drawings. (`--output-format geojson-seq` writes one Feature per line with deterministic ordering; the sidecar report preserves output-format and local-coordinate metadata.)

Exit condition: common civil/utility plan geometry converts natively with quantified losses.

## Milestone 4 — blocks and references

- [x] Read block definitions and `INSERT` references. (Native backend; missing definitions and unresolved child handles are failed outcomes, never silent drops.)
- [x] Compose translation, rotation, non-uniform scale, extrusion, and nested transforms. (Affine chain per instance: insertion point, arbitrary-axis orientation of the insert normal, rotation, MINSERT cell offset, scale, block base point. Nesting capped at 16 levels; MINSERT grids emit one feature set per cell with `[row,col]` id suffixes; accumulated scale > 1 adds a chord-error warning to approximated geometry.)
- [x] Detect recursive block references. (Case-insensitive block-name chain check; recursion is a failed INSERT with the block name in the reason.)
- [x] Add `--explode-blocks` and `--preserve-inserts` modes. (Explode is the default and the flag documents the choice; the two flags conflict; both are native-backend-only and rejected elsewhere. The report records `block_mode`.)
- [x] Preserve block path and attributes in feature properties. (`block_path` joined with `/`; feature ids are prefixed by the insert-handle chain so repeated inserts stay unique; attribute values are emitted on an INSERT anchor point in both modes; ATTDEF templates are counted as skipped.)
- [x] Resolve BYLAYER/BYBLOCK metadata where relevant. (Layers: block content on layer "0" takes the insert's effective layer, keeping `source_layer`. Colors: ByLayer resolves through the effective layer's table entry and ByBlock through the enclosing insert, recursively; resolved colors emit `color_index` (ACI) and/or `color_rgb`, unresolvable policies emit the raw policy string in `color`. Linetypes resolve the same way into `linetype`. TEXT/MTEXT `text_rotation_deg` inside blocks is the direction of the transformed baseline, so insert rotations compose; model-space text keeps its stored rotation verbatim.)

Exit condition: nested engineering symbols and repeated structures are spatially correct and traceable.

## Milestone 5 — CRS, units, and calibration

- [x] Read drawing units and require an override when absent or ambiguous. ($INSUNITS is trusted only when unambiguous AND consistent with $MEASUREMENT; otherwise native reprojection demands `--source-units` (m, mm, cm, dm, km, in, ft, usft). Header-derived units carry a warning that header hints are not authoritative for georeferencing. The reference drawing's mm-vs-english inconsistency triggers exactly this override path.)
- [x] Reproject using the Rust `proj` crate. (`--source-crs`/`--target-crs` on the native backend behind `native-reproject`; drawing units scale to meters before the transform under the documented meter-based-projected-source assumption; a vertex PROJ rejects aborts the conversion — no partial mixes.)
- [x] Support local affine calibration from at least two control points. (`--control-point DX,DY=X,Y`, repeatable; deliberately a 4-parameter similarity — full affine could shear engineering geometry, see the module docs. Exact for two points, least squares beyond; conflicts with `--source-crs` and works without PROJ.)
- [x] Add residual/error reporting for three or more control points. (Per-point residuals, RMS, and max error in target-CRS units, reported for every point count; recorded in the report's `native.calibration` block.)
- [x] Reject implausible EPSG:4326 extents unless explicitly overridden. (Georeferenced output targeting WGS 84 fails closed when any coordinate leaves [-180, 180] x [-90, 90], naming the offending feature; `--allow-suspect-extents` overrides.)
- [x] Record axis order, units, source CRS, target CRS, and transformation pipeline. (Report `native.reprojection`: unit + provenance, meters/unit factor, normalized axis order, PROJ pipeline definition and version; the GeoJSON `dwg2geo` foreign member carries the coordinate status: georeferenced, calibrated, or local-unreferenced.)

Exit condition: output positioning is explicit, reproducible, and sanity checked.

## Milestone 6 — validation and quality gates

- [x] Compare source entity histogram against converted/skipped histogram. (Report `native.accounting`: every top-level model-space entity must reach exactly one outcome — converted, skipped, failed, or INSERT expansion; a nonzero `unaccounted` is surfaced as a converter-bug warning, never hidden.)
- [x] Report bounding boxes before and after transformation. (`native.bbox_drawing` and `native.bbox_output` as [min_x, min_y, max_x, max_y]; identical for local-coordinate output.)
- [x] Detect NaN, infinite, duplicate, empty, and degenerate geometries. (Output-side `native.geometry_checks` pass over the final features; non-finite/empty/degenerate counts raise converter-bug warnings, duplicate consecutive vertices are counted as informational.)
- [x] Validate polygon ring closure and orientation. (Same pass: every Polygon/MultiPolygon ring is checked for closure, CCW shells/CW holes, and minimum size.)
- [x] Add golden GeoJSON tests with coordinate tolerances. (A synthetic AC1027 fixture pins seven native features; coordinates compare recursively within 1e-9; `UPDATE_GOLDEN=1` regenerates, and a sensitivity test proves the comparator catches 1e-6 drift.)
- [ ] Add property-based tests for arc tessellation and affine transforms.
- [ ] Differentially compare native output with LibreDWG/GDAL on a fixture corpus.
- [ ] Establish acceptable loss thresholds per entity class.
- [ ] Detect spatial outliers relative to the main coordinate cluster (dispersion/percentile based). Motivated by the reference drawing: ~18% of features are title blocks and legends near the drawing origin that reproject into geometry scattered across ±50° of longitude without tripping the global WGS 84 extent check.
- [ ] Validate output containment against a reference boundary polygon (e.g. an IBGE municipal boundary fetched by `scripts/fetch-sorocaba-boundary.sh`); this technique conclusively identified the reference drawing's CRS.

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
