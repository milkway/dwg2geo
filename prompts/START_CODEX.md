Continue the `dwg2geo` Rust project in this repository.

First read `AGENTS.md` and every document it lists. Then inspect the repository and run:

```bash
git status --short
cargo fmt --check
cargo check
cargo test
```

Your first objective is to finish Milestone 0 and stabilize Milestone 1, not to rewrite the architecture.

Work in this order:

1. Fix any compilation, formatting, or dependency-version errors while preserving the documented CLI behavior.
2. Add tests for DWG signature inspection, unknown signatures, short/corrupt files, output overwrite validation, and the requirement for either `--source-crs` or `--allow-local-coordinates`.
3. Improve subprocess errors so they include the command, exit status, and bounded stderr.
4. Add `doctor --json` with tool availability and versions, while keeping human-readable output as the default.
5. Add a deterministic conversion sidecar report skeleton, even when a subprocess fails.
6. Update `docs/PLAN.md` checkboxes and documentation for everything completed.

Constraints:

- Do not commit or copy proprietary DWG files.
- Do not implement a DWG parser from scratch.
- Do not begin broad native entity conversion until the external MVP is tested.
- Never silently assume a CRS.
- Keep public behavior backward compatible with the command examples in `README.md`.
- Run `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, and `cargo test` before finishing.

At the end, provide a concise summary of files changed, commands run, failures or limitations, and the exact next unchecked roadmap item.
