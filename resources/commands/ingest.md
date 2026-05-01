Read `.claude\tools\wiki\INGEST_FLOW.md`, then use the `wiki` mcp service to ingest all data in the folder `.wiki/ingest` into the wiki database.
If ingestion of a document is successful, delete it from `.wiki/ingest`.

After the batch finishes, run `/learn` (ingest-time mode) over the doc IDs created in this run to wikilink mentions and fold duplicates. See `.claude/skills/learn/SKILL.md`.
