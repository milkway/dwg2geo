# Architecture

## Data flow

```text
DWG input
   |
   +--> header inspector (always available)
   |
   +--> external backend: LibreDWG -> DXF/GeoJSON -> GDAL reprojection
   |
   +--> native backend: acadrust -> internal CAD model
                                      |
                                      +--> block expansion / OCS-WCS
                                      +--> curve tessellation
                                      +--> geometry validation
                                      +--> CRS/unit transformation
                                      +--> GeoJSON / GeoJSONSeq writer
                                      +--> conversion report
```

## Modules

### CLI

Owns argument parsing and user-facing validation. It must reject ambiguous CRS behavior before executing expensive work.

### Header inspector

Reads only stable file-level information such as DWG signature, size, and hash. It must work even when optional CAD libraries are unavailable.

### Backend adapters

`external` invokes independent command-line tools. `native` adapts `acadrust`. Both must produce shared diagnostics and, eventually, the same internal conversion result contract.

### Internal CAD model

The native parser adapter should normalize library-specific types into project-owned structures. Suggested structures:

```rust
struct CadFeature {
    source_id: SourceEntityId,
    layer: String,
    entity_type: CadEntityType,
    geometry: CadGeometry,
    properties: PropertyMap,
    provenance: Provenance,
    warnings: Vec<Diagnostic>,
}
```

`CadGeometry` should initially retain curve primitives instead of prematurely reducing every entity to line strings:

```text
Point
Line
Polyline with bulge segments
Arc
Circle
Ellipse
Spline
Polygon rings
Text anchor
```

This permits a centralized tessellator and consistent tolerances.

### Transform pipeline

Apply transformations in an explicit order:

1. entity-local coordinates;
2. entity OCS -> WCS;
3. nested block/INSERT transforms from inner to outer;
4. drawing-unit normalization;
5. local control-point affine transform, when configured;
6. source CRS -> target CRS reprojection;
7. target sanity checks and dimension policy.

Every stage should be representable in the report.

### Tessellation

Curved CAD geometry has no direct RFC 7946 equivalent. Tessellation must support:

- a maximum chord-error tolerance in source units;
- a maximum angular step as a safety cap;
- deterministic vertex generation;
- closed-ring consistency;
- minimum and maximum segment guards;
- diagnostics when a tolerance cannot be respected.

### GeoJSON writer

Default output is RFC 7946 GeoJSON in EPSG:4326. For explicitly accepted local coordinates, mark the output and report clearly as non-geographic/local. Consider GeoJSON Text Sequences for streaming large drawings.

### Conversion report

A sidecar report should include:

- source path basename, size, signature, hash;
- tool and library versions;
- CLI options;
- detected units and CRS assumptions;
- source entity histogram;
- converted, approximated, skipped, and failed histograms;
- input and output bounds;
- warning/error samples with entity identifiers;
- elapsed time and peak-memory measurement when available.

## Error policy

- File-level corruption or impossible configuration: fail the command.
- Unsupported individual entity: skip it, count it, and report it unless strict mode is enabled.
- Geometry repair: preserve original diagnostics and mark the feature.
- CRS ambiguity: fail unless an explicit local-coordinate option is present.

## Performance

Do not require the entire GeoJSON string in memory. The long-term writer should stream features. Native parsing may still require a document model depending on `acadrust`, but post-parse geometry should be processed incrementally where possible.
