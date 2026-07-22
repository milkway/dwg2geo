# Future command dispositions

## ADR-FC-001 — Record the disposition of candidate commands

**Status:** accepted.

The project charter lists future commands that "may include" those below; it does not require each candidate to become a standalone command. The current disposition therefore does not breach the implementation contract, but is recorded here so later work does not have to infer intent from the roadmap.

| Candidate | Disposition |
|---|---|
| `layers <FILE>` | Shipped. It reports layer metadata and entity counts by type and space. |
| `entities <FILE>` | Subsumed by the `inspect` entity histogram together with `layers`; a separate command is not planned. |
| `validate <GEOJSON> --against <REPORT>` | Partially shipped through conversion-time `--validate-boundary` and the report's accounting, geometry-check, bounds, outlier, and boundary self-audit blocks. A standalone post-conversion validator remains open. |
| `calibrate <FILE> --control-point ...` | Shipped as `convert --control-point "DX,DY=X,Y"` options with residuals recorded in the report. A standalone calibration command remains open. |

Revisit the two open standalone commands only when they offer a workflow that cannot be expressed clearly and audibly through `convert` and its sidecar report.
