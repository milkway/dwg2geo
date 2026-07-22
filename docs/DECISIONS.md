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

## ADR-009 — PROJ reprojection is a separate Cargo feature

**Status:** accepted.

`native-backend` originally pulled in the `proj` crate, but `proj-sys` needs system PROJ >= 9.6 or a from-source build requiring cmake and sqlite3. Milestone 2 native inspection has no reprojection need, so `dep:proj` moved to a dedicated `native-reproject` feature (which enables `native-backend`). Milestone 5 work happens behind that feature; inspection stays buildable on machines without PROJ.

## ADR-010 — Failsafe parsing must recover something to count

**Status:** accepted.

`acadrust`'s failsafe mode returns an empty default document even for garbage input. The native backend therefore parses strictly first and retries in failsafe mode only on failure; a failsafe result is accepted only if it contains entities or non-default layers, is labeled `failsafe_recovery`, and carries the strict-parse error. An empty recovery is reported as a parse failure, never as a valid empty drawing.

## ADR-011 — Header unit hints are not authoritative for georeferencing

**Status:** accepted.

`$INSUNITS` describes intended insertion/plot units, not the georeferencing of model-space coordinates: engineering drawings routinely carry UTM-metre coordinates while the header says millimetres (the reference drawing does exactly this, and its `$MEASUREMENT` contradicts `$INSUNITS`). Auto-scaling by the header would silently corrupt such files by a factor of 1000. Native reprojection therefore trusts the header only when it is unambiguous and internally consistent — and even then warns and records the provenance — and demands an explicit `--source-units` override otherwise. Unit scaling assumes a meter-based projected source CRS; the WGS 84 extent check backstops gross unit/CRS mistakes.

## ADR-012 — Control-point calibration is similarity-only

**Status:** accepted.

The roadmap phrase "local affine calibration" is implemented as a 4-parameter similarity (Helmert) fit, not a 6-parameter affine. A full affine can shear and scale axes independently, distorting angles and proportions of engineering geometry in ways no residual report would make obvious. Rotation, uniform scale, and translation establish a local georeference without introducing distortions; residuals, RMS, and max error quantify how well the drawing actually fits that model.

## ADR-013 — Doctor health requires both external routes

**Status:** accepted.

The external backend has two documented capabilities: local-coordinate export requires `dwgread`, while CRS-explicit reprojection requires both `dwgread` and `ogr2ogr`. `doctor` reports both route capabilities explicitly, and its overall `healthy` value and exit status are successful only when both routes are available. A system with only `dwgread` remains usable for explicitly opted-in local coordinates, but is reported as degraded rather than healthy because the canonical CRS-explicit route is unavailable.
