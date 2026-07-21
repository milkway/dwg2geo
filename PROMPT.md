# Initial coding-agent prompt

Continue the `dwg2geo` Rust project in this repository.

Read `AGENTS.md` first, followed by every document it lists. Validate the starter with:

```bash
git status --short
cargo fmt --check
cargo check
cargo test
```

Fix compilation or dependency drift conservatively, preserving the documented CLI. Then complete the earliest unchecked tasks in Milestone 0 and Milestone 1 of `docs/PLAN.md`. Prioritize tests, explicit CRS validation, structured diagnostics, `doctor --json`, subprocess error quality, and a deterministic conversion report.

Do not implement the DWG binary format, do not commit proprietary drawings, do not silently assume a CRS, and do not start broad native entity conversion before the external backend MVP is tested.

Before finishing, run formatting, Clippy with warnings denied, and all tests. Update roadmap checkboxes and documentation, then summarize changed files, commands run, limitations, and the exact next task.
