# CAD entity mapping

## Default mapping

This policy is exhaustive for all 44 variants of acadrust 0.4.1's `EntityType` enum. Each variant appears in exactly one table. An acadrust upgrade must update both these tables and `tests/entity_policy.rs` before the new dependency is accepted.

### Converted

| acadrust variant | CAD entity | GeoJSON geometry | Key rules |
|---|---|---|---|
| `EntityType::Point` | POINT | Point | Preserve Z as a property or optional third ordinate according to dimension policy. |
| `EntityType::Line` | LINE | LineString | Two coordinates; reject or report zero-length lines. |
| `EntityType::LwPolyline` | LWPOLYLINE | LineString when open; LineString by default or Polygon by option/rule when closed | Expand bulge segments deterministically. Closure alone does not prove area semantics. |
| `EntityType::Polyline` | POLYLINE / VERTEX | LineString or Polygon | Respect 2D/3D flags and closure. |
| `EntityType::Polyline2D` | POLYLINE / VERTEX (heavy 2D) | LineString or Polygon | Respect 2D/3D flags and closure. |
| `EntityType::Polyline3D` | POLYLINE / VERTEX (3D) | LineString or Polygon | Respect 2D/3D flags and closure. |
| `EntityType::Arc` | ARC | LineString | Tessellate by chord error and angular cap. |
| `EntityType::Circle` | CIRCLE | Polygon or closed LineString by option | Use a closed ring; report approximation. |
| `EntityType::Ellipse` | ELLIPSE | LineString or Polygon | Respect start/end parameters. |
| `EntityType::Spline` | SPLINE | LineString | Evaluate knots/control points; report unsupported variants. |
| `EntityType::Hatch` | HATCH | Polygon / MultiPolygon | Extract loops, classify holes, validate rings. Native backend: boundary paths tessellate under the shared chord tolerance (bulges, arcs, elliptic arcs, NURBS spline edges with fit-point fallback); loops nest by even-odd containment (CCW shells, CW holes); edge gaps beyond the tolerance are bridged/closed with repair warnings, invalid loops are dropped and counted, and pattern name/solid flag are kept as properties. |
| `EntityType::Face3D` | 3DFACE | Polygon | Project WCS corners through INSERT placement to XY and warn when dropping non-zero z; collapse the duplicated fourth corner used for triangles, skip faces with fewer than three distinct projected corners, and always emit a closed CCW Polygon regardless of the closed-polyline polygonization option. |
| `EntityType::Text` | TEXT | Point | Store text, rotation, height, style, alignment, and layer. Native backend emits non-default horizontal/vertical alignment (`text_h_align`, `text_v_align`), relative width and oblique angle (`text_width_factor`, `text_oblique_deg`), mirror flags (`text_mirrored_x`, `text_mirrored_y`), and the effective `text_anchor`. Elided defaults are left/baseline, width factor 1, oblique 0, and no mirroring; left/baseline uses the insertion point even if a stray second point exists, while other modes use the alignment point when present. |
| `EntityType::MText` | MTEXT | Point | Store text, rotation, height, style, alignment, and layer. Native backend emits non-default attachment (`text_attachment`), direction (`text_direction`), reference width (`text_width`), and line spacing (`text_line_spacing_factor`, `text_line_spacing_style`). acadrust 0.4.1 exposes R2018+ column type/count/width/gutter plus flow, auto-height, and per-column heights; these are preserved in `text_columns` when columns are active. Elided defaults are top-left, left-to-right, width 10, factor 1/at-least spacing, and no columns. |
| `EntityType::Insert` | INSERT | Expanded child features or Point/reference | Compose nested transforms and preserve block path. Native backend: expanded by default (translation, normal orientation, rotation, non-uniform scale, MINSERT grids, nesting up to 16 levels; recursion and missing definitions fail with reasons); `--preserve-inserts` emits anchor Points with `block_name` and `attributes`; block content on layer "0" inherits the insert's effective layer (`source_layer` keeps the original). |
| `EntityType::Solid` | SOLID | Polygon | Validate vertex ordering. Native backend: SOLID converts as a CCW Polygon with the DXF bow-tie order (1-2-4-3) untwisted, OCS corners lifted via the arbitrary axis, duplicate triangle corners collapsed, and degenerate solids skipped; TRACE is not exposed by acadrust 0.4.1. |

### Deliberately unsupported with policy

These variants are intentionally not candidates for ordinary GeoJSON feature conversion. They are still counted and reported with handles and reasons, so the policy does not permit silent loss.

| acadrust variant | CAD entity | Policy and reason |
|---|---|---|
| `EntityType::Ray` | RAY | Report-only. A semi-infinite line has no finite GeoJSON equivalent; conversion would require explicit clipping bounds. |
| `EntityType::XLine` | XLINE | Report-only. An infinite construction line has no finite GeoJSON equivalent; conversion would require explicit clipping bounds. |
| `EntityType::Solid3D` | 3DSOLID | Report-only. Preserve counts and handles; ACIS/modeler solids have no lossless GeoJSON representation and must not be presented as equivalent 2D geometry. |
| `EntityType::Region` | REGION | Report-only. Preserve counts and handles; ACIS planar regions require an explicit boundary-extraction policy before any 2D representation is trustworthy. |
| `EntityType::Body` | BODY | Report-only. Preserve counts and handles; ACIS/modeler bodies have no lossless GeoJSON representation. |
| `EntityType::Surface` | ACAD_SURFACE family | Report-only. Preserve counts and handles; modeler surfaces require a separately designed projection or boundary-extraction policy. |
| `EntityType::Block` | BLOCK | Structural block-definition start marker; process the definition through INSERT expansion, never emit the marker as a standalone feature. |
| `EntityType::BlockEnd` | ENDBLK | Structural block-definition end marker; never emit as a standalone feature. |
| `EntityType::Seqend` | SEQEND | Structural end-of-sequence marker; never emit as a standalone feature. |
| `EntityType::AttributeDefinition` | ATTDEF | Block attribute template, not an instance value. Report it and use it only while interpreting INSERT attributes; never emit it as standalone geometry. |
| `EntityType::Unknown` | UNKNOWN | Report-only with raw type information, handle, layer, and location context when available; unknown data must never be guessed into geometry. |

### Not yet converted

These variants currently produce explicit skipped/report diagnostics. The intended mapping below reserves a direction for future work without claiming present support.

| acadrust variant | CAD entity | Intended future mapping or report policy | Reference civil drawing |
|---|---|---|---|
| `EntityType::Helix` | HELIX | Tessellated LineString after defining 3D-to-GeoJSON dimension and curve-tolerance behavior. | Not recorded. |
| `EntityType::Dimension` | DIMENSION (linear, aligned, angular, radial, diametric, ordinate, arc-length) | Expanded graphics and/or semantic properties; remain report-only until dimension-block and measurement semantics can be preserved rather than silently flattened. | Not recorded. |
| `EntityType::Viewport` | VIEWPORT | Report-only initially; retain paper/model-space viewport metadata rather than treating the display window as drawing geometry. | Not recorded. |
| `EntityType::AttributeEntity` | ATTRIB | Attach instance values to the owning INSERT or preserved INSERT anchor; report orphan attributes. | Not recorded. |
| `EntityType::Leader` | LEADER | LineString leader geometry plus annotation linkage and style properties. | Present: 25 entities. |
| `EntityType::MultiLeader` | MULTILEADER | MultiLineString/LineString leader geometry plus text or block-content properties and linkage. | Present: 82 entities. |
| `EntityType::MLine` | MLINE | MultiLineString of constituent lines after offsets, joins, caps, and style semantics are implemented. | Not recorded. |
| `EntityType::Mesh` | MESH | Polygon/MultiPolygon faces after an explicit mesh projection, winding, and Z policy is defined. | Not recorded. |
| `EntityType::RasterImage` | IMAGE | Footprint Polygon with image-reference, clipping, and transform properties; do not embed or imply conversion of raster pixels. | Not recorded. |
| `EntityType::Table` | ACAD_TABLE | Report-only initially; a later semantic mapping may emit an anchor plus structured cell content. | Not recorded. |
| `EntityType::Tolerance` | TOLERANCE | Point annotation with geometric-dimensioning-and-tolerancing text and style properties. | Not recorded. |
| `EntityType::PolyfaceMesh` | POLYFACE mesh | Polygon/MultiPolygon faces after mesh topology, projection, winding, and Z policies are defined. | Not recorded. |
| `EntityType::Wipeout` | WIPEOUT | Footprint Polygon with clipping and mask-semantics properties. | Not recorded. |
| `EntityType::Shape` | SHAPE | Point/reference or expanded vector geometry only when the referenced SHX definition can be resolved; otherwise report-only. | Not recorded. |
| `EntityType::Underlay` | PDF/DWF/DGN UNDERLAY | Footprint Polygon with external-reference, clipping, and transform properties; do not imply conversion of referenced content. | Not recorded. |
| `EntityType::Ole2Frame` | OLE2FRAME | Report-only initially, with footprint and object metadata when available; never embed opaque object data in GeoJSON. | Not recorded. |
| `EntityType::PolygonMesh` | polygon mesh | Polygon/MultiPolygon faces after mesh topology, projection, winding, and Z policies are defined. | Not recorded. |

## Feature properties

Recommended minimum properties:

```json
{
  "cad_entity_type": "LWPOLYLINE",
  "cad_layer": "EIXO_PROJ",
  "cad_handle": "1A2B",
  "cad_space": "model",
  "cad_block_path": ["BLOCO_A", "SUB_BLOCO"],
  "cad_closed": true,
  "cad_approximated": true,
  "cad_warning_codes": ["CURVE_TESSELLATED"]
}
```

Optional style fields may include color, line type, line weight, text style, and visibility. Keep style separate from geometry correctness.

The native backend emits resolved style properties on every feature: `color_index` (ACI 1-255) and/or `color_rgb` (`#RRGGBB`) after resolving ByLayer through the effective layer and ByBlock through the enclosing insert chain; `linetype` resolved the same way. Policies that cannot be resolved (missing layer, ByBlock outside a block) are emitted verbatim as `color`/`linetype` strings — never silently dropped.

## Polygon policy

A closed CAD curve is not automatically an area feature. Support policies such as:

- `line`: preserve closed polylines as linework;
- `polygon`: treat every valid closed polyline as a polygon;
- `layer-rule`: polygonize only configured layers/patterns;
- `hatch-only`: derive polygons only from hatches.

Default to the least assumptive behavior.

## Unsupported entities

Unsupported entities must produce a diagnostic containing, when available:

- entity type;
- handle;
- layer;
- block path;
- reason code;
- whether the entity was skipped, partially converted, or approximated.
