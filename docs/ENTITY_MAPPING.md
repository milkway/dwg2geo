# CAD entity mapping

## Default mapping

| CAD entity | GeoJSON geometry | Key rules |
|---|---|---|
| POINT | Point | Preserve Z as a property or optional third ordinate according to dimension policy. |
| LINE | LineString | Two coordinates; reject or report zero-length lines. |
| LWPOLYLINE open | LineString | Expand bulge segments deterministically. |
| LWPOLYLINE closed | LineString by default; Polygon by option/rule | Closure alone does not prove area semantics. |
| POLYLINE / VERTEX | LineString or Polygon | Respect 2D/3D flags and closure. |
| ARC | LineString | Tessellate by chord error and angular cap. |
| CIRCLE | Polygon or closed LineString by option | Use a closed ring; report approximation. |
| ELLIPSE | LineString or Polygon | Respect start/end parameters. |
| SPLINE | LineString | Evaluate knots/control points; report unsupported variants. |
| HATCH | Polygon / MultiPolygon | Extract loops, classify holes, validate rings. |
| 3DFACE | Polygon | Apply configured projection/dimension policy. |
| TEXT / MTEXT | Point | Store text, rotation, height, style, alignment, and layer. |
| INSERT | Expanded child features or Point/reference | Compose nested transforms and preserve block path. Native backend: expanded by default (translation, normal orientation, rotation, non-uniform scale, MINSERT grids, nesting up to 16 levels; recursion and missing definitions fail with reasons); `--preserve-inserts` emits anchor Points with `block_name` and `attributes`; block content on layer "0" inherits the insert's effective layer (`source_layer` keeps the original). |
| DIMENSION | Expanded graphics and/or semantic properties | Initially report unsupported rather than silently flattening incorrectly. |
| SOLID / TRACE | Polygon | Validate vertex ordering. |
| RAY / XLINE | Unsupported by default | Infinite geometry needs clipping bounds. |
| ACIS 3D solids | Unsupported initially | Report counts and handles; do not pretend to preserve solids in GeoJSON. |

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
