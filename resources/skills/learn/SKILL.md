---
name: learn
description: Connect wiki docs by replacing bare entity mentions with `[[wikilinks]]`, deduping overlapping content into canonical entities, and linking contributors via `Consolidates` reasons. Run as standalone `/learn` pass, or as a sub-step inside `ingest_thought` / `ingest_entity` and before query synthesis. Use when the wiki feels like disconnected islands, after a large ingest batch, or when the user asks to "connect / link / consolidate / dedupe" wiki content.
tags:
  - skill
  - wiki
  - knowledge-base
  - linking
---

# Learn — entity linking + dedupe pass

Wiki grows as disconnected docs. `learn` walks docs, finds entity mentions, rewrites them as Obsidian `[[wikilinks]]`, and folds duplicate content into the canonical entity. Result: dense graph, less repetition, fact lives in one place.

Reads `.claude/tools/wiki/INGEST_FLOW.md` for vocab. Skill assumes `wiki` MCP is up.

## Three modes

| Mode | Trigger | MCP tool |
|------|---------|----------|
| **batch** | `/learn [limit] [purpose]` | `wiki.learn_pass({limit, purpose, dry_run})` |
| **ingest-time** | called after `ingest_thought` / `ingest_entity` | `wiki.link_doc({doc_type, id, dry_run})` |
| **query-time** | before answering / synthesizing conclusion | parse `[[links]]` from seed bodies, resolve via `link.json`, `get()` linked docs |

`learn_pass` and `link_doc` are native Rust — single call replaces dozens of round-trips. Both accept `dry_run` to preview without writing.

## Inputs

- `limit` — max docs to process per pass (default 25). Bound work, avoid runaway OpenAI cost.
- `purpose` — optional. Restrict to one purpose tag (e.g. `nanite`).
- `dry_run` — log proposed edits, no write. Default off.

## Native algorithm (in Rust, `src/learn.rs`)

Both modes implement the same pipeline:

1. **Build entity index.** `store::list_documents("entities")`, parse `aliases:` from frontmatter (optional), embed each entity body via `classifier::embed_batch` (`text-embedding-3-small`).
2. **Per doc:**
   - Read body via `get_document`.
   - **Link rewrite.** Sort entity titles + aliases by length desc. For each, regex `(?i)\b{escaped}\b` find first occurrence. Skip if inside protected range (fenced code, inline backticks, existing `[[...]]`, markdown links). Replace with `[[<entity-slug>|<surface>]]`. One link per entity per doc.
   - **Paragraph dedupe.** Split on `\n\n`. Embed paragraphs ≥40 chars. Compare cosine vs each entity body. If ≥ `WIKI_DEDUPE_THRESHOLD` (default `0.85`) → drop paragraph, emit `Consolidates` reason `entity_id → doc_id` with body `"absorbed paragraph hash:<fnv64>"`.
   - `update_document` if changed. Reindex via `search::index_document`.
3. **Report** to `.wiki/ingest_log/learn-<ts>.json`.

`run_pass` walks `thoughts` ∪ `conclusions` up to `limit`, optionally filtered by purpose. `link_doc` runs steps 2–3 on a single doc.

### Aliases

Add to entity frontmatter:
```yaml
aliases:
  - Alt Name
  - Acronym
```
Linker matches title + aliases. ≥3 chars required to avoid stop-word noise.

Net effect: ingest writes wikilinked, deduped content from day one.

## Algorithm — query-time expansion

Before answering or running `suggest_conclusion`:

1. Run `search_fulltext` / `search_by_tag` for seeds (existing flow).
2. For each seed, parse `[[links]]` from body. Resolve via `link.json`. `get()` linked docs (cap at depth 1, fanout 5).
3. Feed expanded set to synthesis. Cite all walked IDs.

## Guardrails

- Skip code fences, inline backticks, existing `[[...]]`, markdown links.
- Self-link prevented (entity never links to itself).
- Paragraphs <40 chars not embedded — too noisy for cosine.
- `WIKI_DEDUPE_THRESHOLD` env var (default `0.85`) — distinct from purpose classification threshold (`0.35`).
- Always pair `dry_run: true` first on a fresh vault to inspect proposed merges.
- Contradictions are **not** detected automatically — `learn` only fires on cosine similarity. Manually emit `contradicts` reason after review if needed.

## Output schema

```json
{
  "pass_id": "<rfc3339>",
  "docs_scanned": N,
  "docs_modified": M,
  "links_added": K,
  "paragraphs_merged": P,
  "entity_count": E,
  "purpose_filter": "<tag|null>",
  "dry_run": false,
  "details": [{ "doc_id", "doc_type", "links_added", "paragraphs_merged", "modified" }]
}
```

Dump at `.wiki/ingest_log/learn-<ts>.json`.

## When to run

- After every `/ingest` batch — pass `limit` = number of newly ingested docs.
- Periodic health check — full vault, paginated by `limit`.
- Before writing a conclusion — query-mode only, read-only.
- When user reports "wiki feels disconnected" or "too much duplication".
