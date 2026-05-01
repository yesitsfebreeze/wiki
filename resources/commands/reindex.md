# /reindex — wipe wiki, re-index codebase

Purge the wiki vault, then re-index the entire codebase via the `wiki` MCP service.

## Steps

1. **Confirm with user** before wiping. Wipe is irreversible — destroys all thoughts, entities, reasons, conclusions, purposes.

2. **Purge `.wiki/` vault** (keep `.wiki/purposes/` definitions if user wants — ask):
   - Default (full wipe): `rm -rf .wiki/thoughts .wiki/entities .wiki/reasons .wiki/conclusions .wiki/questions .wiki/ingest`
   - Recreate empty `.wiki/ingest/` after.

3. **Read** `.claude/tools/wiki/INGEST_FLOW.md` for current ingest semantics.

4. **Walk codebase** — collect source files. Skip:
   - `target/`, `node_modules/`, `dist/`, `build/`, `.git/`
   - `.wiki/`, `.claude/tools/wiki/target/`
   - binaries, lockfiles, generated artifacts

5. **For each source file** call `ingest_thought({title: <relpath>, content: <file body>})`. Auto-chunking handles multi-purpose splits.

6. **Periodically** (every ~25 files) run `search_fulltext` for emerging clusters; promote 3+ similar thoughts to entities via `ingest_entity`.

7. **After batch** run `/learn` (ingest-time mode) on the new doc IDs to wikilink mentions and dedupe.

## Notes

- Large repos: batch by directory. Report progress after each batch.
- Respect `OPENAI_API_KEY` — without it everything falls into `general` purpose.
- Do not re-purge mid-run if interrupted; resume from `.wiki/ingest/` state.
