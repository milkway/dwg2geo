# Differential conversion policy

This policy defines the Milestone 6 quality gate between the native converter
and the external local-coordinate route. The integration harness in
`tests/differential.rs` generates three deterministic AC1027 drawings and runs
the compiled `dwg2geo` binary twice for each drawing. The external command uses
LibreDWG's direct GeoJSON writer; GDAL is not invoked for local-coordinate
output.

The baseline was measured with `dwgread 0.14` and GDAL 3.8.4. Tests probe
`dwgread --version` and skip with an explicit message when it is unavailable.

## Corpus baseline

| Drawing | Native features | External features | Required entity counts |
|---|---:|---:|---|
| points and lines | 4 | 4 | `POINT` 2 and `LINE` 2 in each |
| closed and bulged polylines, circle, trailing point | 4 | 4 | `LWPOLYLINE` 2, `CIRCLE` 1, and `POINT` 1 in each |
| text, point, and block INSERT | 3 | 4 | `TEXT` 1, `POINT` 1, and `LINE` 1 in each; native `INSERT` 0, external `INSERT` 1 |

The text/INSERT difference is exact, not a general `±1` allowance. The native
default explodes the INSERT into its transformed child LINE. LibreDWG emits the
untransformed block-definition LINE and a separate INSERT point, so the
external result has exactly one additional feature.

## Acceptance thresholds

- `POINT`, `LINE`, `TEXT`, `LWPOLYLINE`, and `CIRCLE` feature counts must match
  exactly in the applicable corpus drawings: acceptable count loss is zero.
- INSERT handling must match the exact block baseline above. A different total,
  a missing child LINE, or an INSERT-count change is not covered by the policy.
- The total feature-count delta is zero for the primitive and curve drawings.
  It is exactly `external - native = 1` for the text/INSERT drawing.
- Every ordinate of the union coordinate bbox must match within `1e-6` drawing
  units for every drawing.
- The first vertex of every native Point or LineString must occur within `1e-6`
  of some coordinate in the external output. Ordering and feature-to-feature
  pairing are deliberately not required.

Any new divergence beyond these thresholds is a release blocker until it is
explained, documented, and assigned a deliberate new threshold. Thresholds
must not be widened merely to make a failing fixture pass.

## Known representation differences and external-route loss

- Native closed polylines remain closed LineStrings by default; LibreDWG emits
  the closed fixture as a Polygon.
- Native circles are closed, tolerance-controlled LineStrings; LibreDWG emits
  the circle as a densely sampled Polygon. Entity count, first vertex, and
  drawing-wide bounds still have to satisfy the gates above.
- Native bulges are tessellated under the configured chord tolerance.
  LibreDWG's direct GeoJSON output contains only the bulged polyline's stored
  endpoints, losing the arc shape. Vertex counts and intermediate curve
  coordinates are therefore not differential invariants.
- LibreDWG emits block-definition geometry without applying the INSERT
  transform and also emits the INSERT anchor. Native output instead contains
  the transformed exploded child. The block fixture pins the resulting count
  difference and uses containment rather than feature ordering.
- With the generated CIRCLE as the final drawing entity, `dwgread 0.14` appends
  an unseparated null feature and produces malformed JSON. The curve fixture
  includes a trailing comparable POINT so the harness exercises the successful
  comparison path. A malformed external conversion remains a hard failure; it
  is not acceptable loss.

LibreDWG's generated `geocoding.creation_date` is not compared. Fixture source
geometry, insertion order, count maps, and assertions contain no timestamps or
unordered-map dependencies.

## Reference-file context

Milestone 2 found that, on the local proprietary reference drawing, the native
reader counted 219 more entities (about 2.5%) than LibreDWG's DXF export, only
in that direction; 8 of 17 entity types matched exactly and five block
definitions were absent from the DXF. That result motivates the one-directional
loss investigation, but it is not a blanket 2.5% allowance. Supported classes
in this synthetic corpus remain subject to the zero-loss and exact-divergence
thresholds above.
