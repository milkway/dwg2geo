# Packaging generated CLI assets

Every Cargo build generates shell completions and man pages under
`target/<profile>/build/dwg2geo-*/out/assets/`. The build warning prints the
exact directory, which is also exposed to crate compilation as
`DWG2GEO_ASSET_DIR`. The directory contains `dwg2geo.bash`, `_dwg2geo`,
`dwg2geo.fish`, `_dwg2geo.ps1`, `dwg2geo.1`, and man pages for each
subcommand.

Packagers should copy the completion files to their platform's standard bash,
zsh, fish, and PowerShell completion locations. On typical Unix packages these
are `/usr/share/bash-completion/completions/dwg2geo`,
`/usr/share/zsh/site-functions/_dwg2geo`, and
`/usr/share/fish/vendor_completions.d/dwg2geo.fish`. Install all `*.1` files in
`/usr/share/man/man1/`. PowerShell's installation location is package-manager
specific; install `_dwg2geo.ps1` where that package loads completion scripts.
