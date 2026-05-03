# reembed_purposes

Rebuild all purpose `.vec` embedding sidecar files from scratch.

## Params

None.

## Returns

Confirmation string with count: `"Re-embedded N purposes"`.

## Notes

- Drops all existing `.vec` sidecars before rebuilding — do not run while a `learn_pass` or `ingest` is in progress.
- Run after any of:
  - Editing a purpose `description` via `create_purpose` (replace) or manual file edit.
  - Deleting a purpose (removes its stale sidecar).
  - Upgrading the embedding model (model change invalidates all cached vectors).
- Fast for small vaults; proportional to number of purposes × embedding latency.
