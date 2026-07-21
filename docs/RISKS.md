# Risks and mitigations

## DWG coverage risk

A library may claim a DWG generation while still missing particular entity/object combinations.

Mitigation: maintain an entity histogram, a fixture corpus, per-entity diagnostics, and differential comparisons with LibreDWG/GDAL.

## Native-library maturity

`acadrust` is recent and its API or behavior may change.

Mitigation: isolate it behind an adapter and optional Cargo feature; pin versions for releases; test AC1027 and other required generations explicitly.

## Geographic misplacement

DWGs frequently omit usable CRS metadata or use local grids.

Mitigation: fail closed, require source CRS or control points, report transformations, and sanity-check final longitude/latitude extents.

## Units ambiguity

Drawing units can be absent, misleading, or inconsistent with coordinate values.

Mitigation: detect and report units, allow explicit overrides, and compare extents against plausible engineering scales.

## OCS/WCS and extrusion errors

Many apparently simple entities are expressed in an object coordinate system.

Mitigation: implement and test the arbitrary-axis algorithm and transform order before declaring entity support.

## Block transform errors

Nested insertion, rotation, extrusion, and non-uniform scaling can produce subtle positional errors.

Mitigation: use transform matrices, property-based tests, nested fixtures, and recursion detection.

## Curve and topology loss

Arcs, bulges, splines, and hatch loops can be omitted or incorrectly polygonized.

Mitigation: centralized tessellation, configurable tolerances, ring validation, and explicit approximation diagnostics.

## Large-file memory use

Engineering drawings can contain large numbers of entities and expanded blocks.

Mitigation: support GeoJSONSeq streaming, avoid materializing serialized output, add resource measurements, and provide layer filtering.

## False confidence from valid JSON

A valid GeoJSON file may still be spatially or semantically wrong.

Mitigation: generate conversion reports, count reconciliation, bounding-box checks, and visual/independent GIS validation.

## Licensing and redistribution

External tools and Rust dependencies use different licenses.

Mitigation: create a dependency-license report, keep executable invocation separate from library linking, and review release packaging before publication.
