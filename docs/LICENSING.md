# Licensing and distribution boundaries

`dwg2geo` itself is MIT-licensed (`LICENSE-MIT`). The external conversion
backend, however, invokes third-party command-line tools with copyleft or
mixed licenses. This document states exactly what is bundled where, so a
distributor knows which license terms apply to which artifact.

## The tools the external backend uses

| Tool | Command | License | Role |
|---|---|---|---|
| GNU LibreDWG | `dwgread` | **GPL-3.0-or-later** | DWG → DXF/GeoJSON in the external backend |
| GDAL | `ogr2ogr` | MIT/X11-style (permissive) | DXF → GeoJSON reprojection in the external backend |
| PROJ | (linked by GDAL, and by the optional `native-reproject` feature) | X/MIT-style (permissive) | coordinate transformation |

LibreDWG is the only copyleft dependency, and it is the one that governs how
the external backend may be distributed.

## How `dwg2geo` uses these tools: separate processes, not linkage

The external backend **shells out** to `dwgread` and `ogr2ogr` as separate
processes (`std::process::Command`). It does not link against LibreDWG or GDAL,
does not embed their code, and does not ship their binaries. The GPL's copyleft
attaches to LibreDWG and its derivatives; running an independent program that
merely executes `dwgread` at runtime does not make `dwg2geo` a derivative work
of LibreDWG. This is the same boundary as any program that calls a GPL CLI it
found on `$PATH`.

Consequently:

- **The `dwg2geo` source and its own binaries are MIT.** They contain no
  LibreDWG or GDAL code.
- **The external backend only works if the user already has `dwgread`/`ogr2ogr`
  installed.** `dwg2geo doctor` reports whether they are present. We do not
  distribute them.

## Distribution artifacts and their effective license

| Artifact | Contents | Effective license for redistribution |
|---|---|---|
| Prebuilt release binaries (`native-backend`) | only `dwg2geo` | MIT |
| `cargo install` / source build | only `dwg2geo` (+ its Rust deps, all permissive) | MIT |
| Slim container image | only the `dwg2geo` native binary | MIT |
| A hypothetical "full" image bundling `dwgread` | `dwg2geo` **+ GPL-3.0 LibreDWG** | **GPL-3.0** — would have to be distributed under the GPL, with corresponding source offers |

**We deliberately do not ship a container (or any artifact) that bundles
LibreDWG**, precisely to keep every distributed artifact MIT-clean. A user who
wants the external backend installs LibreDWG/GDAL themselves from their OS
package manager; at that point the GPL obligations are between the user and the
LibreDWG distribution, exactly as with any other GPL tool on their machine.

## The native backend has no GPL exposure

The pure-Rust native backend (`--features native-backend`) reads DWG with the
`acadrust` crate and converts entirely in-process. It uses **no LibreDWG and no
GDAL**. Its dependency tree is permissively licensed (see
`THIRD-PARTY-LICENSES.md` / the CycloneDX SBOM for the exact per-crate terms).
The optional `native-reproject` feature adds the `proj`/`proj-sys` crates, which
link the permissively licensed PROJ library — still no copyleft.

## Summary

- Ship and redistribute the native binaries, `cargo`-installed builds, and the
  slim container freely under MIT.
- Do not bundle LibreDWG into a distributed artifact unless you intend to
  distribute that artifact under the GPL-3.0.
- The external backend remains fully usable — it just relies on tools the user
  installs, which is where any GPL obligations correctly live.
