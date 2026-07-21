Implement Milestone 2 — native inspection — after Milestone 1 is complete.

Read `AGENTS.md`, `docs/ARCHITECTURE.md`, and `docs/PLAN.md`. Enable the existing `native-backend` feature and use `acadrust` only behind an adapter boundary.

Deliver a vertical slice that:

1. opens supported DWGs through `acadrust::io::dwg::DwgReader`;
2. extends `inspect` with layer count, block count, model/paper-space counts, entity histogram, drawing units, and extents when available;
3. adds `dwg2geo layers <FILE> [--json]`;
4. never exposes `acadrust` types outside the backend adapter;
5. reports parse gaps and unsupported objects explicitly;
6. includes synthetic or redistributable tests and does not commit the proprietary reference drawing;
7. compares local reference-file counts with LibreDWG where possible and records only non-sensitive aggregate results.

Do not implement full GeoJSON entity conversion in this milestone. Finish with formatting, Clippy, tests, docs, and updated roadmap checkboxes.
