# Local sample drawings

Place local test drawings and any data derived from them in this directory.
Everything here is git-ignored except this `README.md`, so client drawings and
their metadata, histograms, and validation notes never reach the repository.

Expected local reference filename:

```text
_Corredor Sul.dwg
```

Regenerate the observed metadata and the aggregate entity histogram locally
with `dwg2geo inspect <file> --json`. Verify the SHA-256 before treating a
local file as the same reference.

Never commit or redistribute the drawing (or its derived data) unless you have
explicit rights to do so.
