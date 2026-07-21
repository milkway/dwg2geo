# Initial architectural decisions

## ADR-001 — Do not implement the DWG binary parser

**Status:** accepted.

Use GNU LibreDWG as an external backend and `acadrust` as the intended native Rust backend. DWG parsing is too complex and version-sensitive to be a sensible first-party implementation goal.

## ADR-002 — Fail closed when the CRS is unknown

**Status:** accepted.

The converter requires `--source-crs` for geographically referenced output. Raw CAD coordinates are allowed only with `--allow-local-coordinates`, and the report must state that no geographic CRS was established.

## ADR-003 — External backend before native geometry conversion

**Status:** accepted.

The external path provides immediate practical value and a differential reference while native support matures. It also isolates the CLI, reporting, CRS, and validation design from parser-specific work.

## ADR-004 — Keep a CAD-neutral intermediate model

**Status:** accepted for the native backend.

`acadrust` entity types are adapted at the boundary. Conversion, transforms, validation, and output operate on project-owned types. This limits dependency coupling and makes differential testing possible.

## ADR-005 — Curves are approximated under an explicit tolerance

**Status:** accepted.

GeoJSON does not represent CAD arcs and splines directly. Approximation parameters and warning metadata are part of the conversion contract, not hidden implementation details.

## ADR-006 — Closed polylines are not polygons by default

**Status:** accepted.

Closure does not establish semantic area. Polygon conversion is controlled by an option or rule, with hatches treated as stronger evidence of area topology.

## ADR-007 — The proprietary reference drawing is local-only

**Status:** accepted.

Store only non-sensitive technical metadata in git. Validation against the real drawing is a documented local step. Do not embed or redistribute it without explicit rights.

## ADR-008 — Licensing requires a distribution review

**Status:** proposed.

Invoking an installed LibreDWG executable is architecturally separate from linking its library, but packaging and distribution choices still require a license review. The native `acadrust` path and project license should also be checked before public release. Do not make legal guarantees in documentation.
