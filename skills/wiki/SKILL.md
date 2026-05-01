---
name: wiki
description: Maintain a persistent Obsidian-vault knowledge base via the `wiki` MCP server. Single vault at .wiki/ with topical separation via purpose tags, OpenAI-embedding-based auto-classification on ingest. Use when ingesting new sources, answering questions from the wiki, linking nodes, or running periodic health checks.
tags:
  - skill
  - wiki
  - knowledge-base
---

# Wiki Operator

Single Obsidian vault at `.wiki/`. Topics separated by **purpose tags**, not folders. Each doc gets one type tag (`thought|entity|reason|question|conclusion`) + one purpose tag (`nanite|xpbd|phyons|general|...`). Ingest auto-classifies content via OpenAI embeddings.

Always read `.claude/tools/wiki/INGEST_FLOW.md` before ingesting.

## Layout

```
.wiki/
  purposes/<tag>.md           — Purpose definition (drives classification)
  purposes/<tag>.vec          — Cached OpenAI embedding
  thoughts/                   — Atomic facts from sources
  entities/                   — Consolidated concepts
  reasons/                    — Directed edges between nodes
  questions/                  — Open questions
  conclusions/                — Synthesized knowledge
  ingest_log/                 — Audit trail
  ingest/                     — Inbox (drop sources here, `/ingest` drains it)
  .search/                    — Tantivy FTS index
  link.json                   — id → filename map
```

## Session start

No bootstrap needed — the vault is plain Markdown + frontmatter, edited live by the MCP. Before substantive work:

1. `list_purposes()` — confirm topical buckets
2. `search_by_tag({tag: <topic>})` — pull relevant docs for the task

## Ingest workflow

When the user adds files to `.wiki/ingest/`:

1. For each source file, extract atomic claims.
2. `ingest_thought({title, content})` per claim. Content is auto-chunked by purpose; multi-purpose content yields a parent + children + `PartOf` reasons.
3. After thoughts, identify recurring concepts. Promote to entities: `ingest_entity({title, content})`.
4. Manually link entities to contributing thoughts: `ingest_reason({from_id, to_id, kind: "Consolidates", body})`.
5. Delete the source from `.wiki/ingest/` once successfully ingested.
6. Run `learn` skill (ingest-time mode) on the new docs — wikilink mentions, fold duplicate paragraphs into existing entities. See `.claude/skills/learn/SKILL.md`.

Pass `purpose_hint` only when you are certain of the bucket (skips one OpenAI call).

## Query workflow

Before answering a substantive question or making a code change:

1. `search_by_tag({tag})` for each relevant topical purpose.
2. `search_fulltext({query})` for keyword sweep across all purposes.
3. For chunked docs, walk back to parents: `search_reasons_for({node_id: <child>, direction: "to"})` and look for `PartOf` reasons.
4. Run `learn` skill (query-time mode) on seeds — expand via `[[wikilinks]]` depth-1, fanout-5.
5. Cite doc IDs in your answer.
6. If the answer adds durable insight, propose `ingest_conclusion({title, body})`.

## Open-question loop

1. `list_open_questions()` — periodic
2. For each: `find_answers({question_id})` → review candidates → `ingest_reason({from_id: question_id, to_id: candidate, kind: "Answers", body})` → `mark_question({question_id, status: "resolved"})`
3. When an entity has reasons + ≥2 resolved questions: `suggest_conclusion({entity_id})` → `ingest_conclusion()` if green-lit.

## Tool reference

### Purpose
- `list_purposes()`
- `create_purpose({tag, title, description})`
- `delete_purpose({tag})`
- `reembed_purposes()`

### Ingest (all accept optional `purpose_hint`)
- `ingest_thought({title, content})`
- `ingest_entity({title, content})`
- `ingest_reason({from_id, to_id, kind, body})`
- `ingest_question({body})`
- `ingest_conclusion({title, body})`

### Read
- `get({doc_type, id})`
- `list({doc_type})`
- `list_open_questions()`
- `list_ingest_log()`

### Search
- `search_fulltext({query})`
- `search_by_tag({tag})`
- `search_reasons_for({node_id, direction: "from"|"to"|"both"})`

### Modify
- `update({doc_type, id, content?, tags?})`
- `delete_doc({doc_type, id})`
- `mark_question({question_id, status})`

### Workflow
- `find_answers({question_id})`
- `suggest_conclusion({entity_id})`

### Extract
- `extract_pdfs({paths})`
- `extract_youtube({ids})`

## Environment

- `WIKI_PATH` — vault root (default `./.wiki`)
- `OPENAI_API_KEY` — required for ingest classification
- `WIKI_SIMILARITY_THRESHOLD` — cosine threshold (default `0.35`)

## Guardrails

- `.wiki/ingest/` is an inbox — drains on successful ingest.
- Do not pass a `store` parameter — there are no stores.
- Do not rename reason files (`<from>-<kind>-<to>`); slug is deterministic.
- Each doc gets exactly one purpose tag. Multi-purpose content auto-splits into chunks.
- Keep contradictions visible — link via `contradicts` reason, do not delete prior claims.
- Do not create entities without supporting thoughts.
- After MCP signature changes, restart the MCP server/client session.
