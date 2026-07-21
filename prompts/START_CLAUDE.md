Open and follow `CLAUDE.md`, then treat `AGENTS.md` as the canonical contract.

Begin by validating the repository with `cargo fmt --check`, `cargo check`, and `cargo test`. Repair the starter conservatively if dependency or API versions have drifted.

Complete the earliest unchecked tasks in Milestone 0 and Milestone 1 of `docs/PLAN.md`. Prioritize tests, structured diagnostics, `doctor --json`, subprocess error quality, and a deterministic conversion report. Do not start a DWG parser or broad native geometry conversion yet.

Preserve these safety properties:

- source CRS is mandatory unless local coordinates are explicitly accepted;
- no unsupported entity or conversion failure may be silent;
- proprietary drawings stay outside git;
- existing CLI examples remain valid.

Work through the tasks without pausing for routine confirmations. Run formatting, Clippy with warnings denied, and all tests before reporting results. Update the plan checkboxes and relevant docs as part of the same change.
