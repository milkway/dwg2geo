# Distribution

## Slim native container

The distributed container is deliberately native-only. Its application payload
is the `dwg2geo` release binary built with `--features native-backend`, running
as a non-root user on `gcr.io/distroless/cc-debian12`.

It does **not** contain LibreDWG (`dwgread`), GDAL (`ogr2ogr`), PROJ, or their
shared libraries. Consequently, the external backend is unavailable in this
image, and the separate `native-reproject` feature is not enabled. LibreDWG is
excluded deliberately so the slim artifact does not redistribute a
GPL-3.0-or-later component. Users who need the external route must install and
run LibreDWG/GDAL outside this image and review their distribution obligations.

Build and inspect the image locally:

```bash
docker build -t dwg2geo:test .
docker run --rm dwg2geo:test doctor
```

`doctor` checks the external-tool routes, so it reports `dwgread` and `ogr2ogr`
as missing and exits nonzero inside this image. That result is expected; it does
not mean the native backend is unavailable. Native commands can read/write host
files through an explicitly mounted directory.

## SBOM

Install the CycloneDX Cargo plugin once, then generate the SBOM for the shipped
feature set:

```bash
cargo install cargo-cyclonedx --locked
./scripts/gen-sbom.sh
jq empty sbom.cdx.json
```

The script runs `cargo cyclonedx --format json --features native-backend`, names
the result `sbom.cdx.json`, and validates that it is JSON. Generate it from the
locked release revision. Do not publish an SBOM carried over from a different
`Cargo.lock`.

## Dependency-license report

Install `cargo-about` once and regenerate the attribution report whenever
`Cargo.lock`, enabled release features, or `about.toml` changes:

```bash
cargo install cargo-about --locked
./scripts/gen-licenses.sh
git diff --exit-code -- THIRD-PARTY-LICENSES.md
```

`about.toml` accepts the project's reviewed permissive licenses plus MPL-2.0.
An unsatisfied or newly introduced license expression makes `cargo-about`
fail. The generated `THIRD-PARTY-LICENSES.md` contains the dependency
attributions and license texts; it should ship beside release binaries.

## License review for the native binary

The following table summarizes the important packages in the locked
`native-backend` graph. It is an engineering inventory, not legal advice.

| Component | License expression | Distribution note |
|---|---|---|
| `dwg2geo 0.1.0` | MIT | Project license. |
| `acadrust 0.4.1` | MPL-2.0 | Weak/file-level copyleft. Preserve notices and review MPL source-availability obligations for covered files; it does not make LibreDWG part of the image. |
| `geojson 1.0.0` | MIT or Apache-2.0 | Permissive. |
| `geo-types 0.7.19` | MIT or Apache-2.0 | Permissive. |
| `clap 4.6.4` | MIT or Apache-2.0 | Permissive. |
| `serde 1.0.229`, `serde_json 1.0.151` | MIT or Apache-2.0 | Permissive. |
| `anyhow 1.0.104`, `sha2 0.10.9`, `tempfile 3.27.0` | MIT or Apache-2.0 | Permissive. |
| `nalgebra 0.32.6` | BSD-3-Clause | Permissive transitive dependency of `acadrust`. |
| `encoding_rs 0.8.35` | (Apache-2.0 or MIT) and BSD-3-Clause | Both permissive terms apply. |
| `unicode-ident 1.0.24` | (MIT or Apache-2.0) and Unicode-3.0 | Permissive, with Unicode notice terms. |
| `proj 0.31.0`, `proj-sys 0.27.0` | MIT or Apache-2.0 | Only selected by `native-reproject`; absent from this container and its SBOM/license graph. A separately supplied or built PROJ library needs its own review. |
| GNU LibreDWG | GPL-3.0-or-later | External executable only; absent from the slim image and native binary. |
| GDAL | MIT-style/X license | External executable only; absent from the slim image and native binary. |

The locked native runtime graph contains no GPL, LGPL, or AGPL Rust dependency.
`acadrust`'s MPL-2.0 is the only copyleft license in that graph and is the main
distribution-review item. Several transitive crates offer uncommon licenses
such as 0BSD, Unlicense, BSL-1.0, or Zlib as alternatives, but their SPDX `OR`
expressions can be satisfied using licenses allowed by `about.toml`; compound
`AND` expressions such as `encoding_rs` and `unicode-ident` remain fully
represented in the generated report.

## Recommended CI integration

The CI/release-owning branch can adapt these steps into its workflow. Pinning
tool versions is recommended when that branch establishes its release-tool
update policy.

```yaml
- name: Install distribution audit tools
  run: |
    cargo install cargo-cyclonedx --locked
    cargo install cargo-about --locked

- name: Generate and validate SBOM
  run: |
    ./scripts/gen-sbom.sh
    jq empty sbom.cdx.json

- name: Check third-party license report
  run: |
    ./scripts/gen-licenses.sh
    git diff --exit-code -- THIRD-PARTY-LICENSES.md

- name: Build slim native container
  run: docker build --tag dwg2geo:ci .

- name: Verify expected slim-container doctor result
  shell: bash
  run: |
    set +e
    docker run --rm dwg2geo:ci doctor
    doctor_status=$?
    set -e
    test "$doctor_status" -eq 1
```

Upload `sbom.cdx.json` as a release artifact rather than committing it unless
the release process guarantees regeneration from the exact locked revision.
