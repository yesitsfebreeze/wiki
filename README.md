# WIKI - Obsidian Synthetik Knowledge Base

## Architecture

MCP server managing a single Obsidian vault via stdio.
Rust implementation, standalone, fast, with embedded full-text search (tantivy) and OpenAI-embedding-based purpose classification.
Developed in `./.claude/tools/wiki`.

## Single Vault, Many Purposes

One vault per project. Topical separation is achieved via **purpose tags**, not separate vaults.
Each document gets exactly **one purpose tag** (unique among purposes) plus exactly **one type tag** (`#thought | #entity | #reason | #question | #conclusion`) plus optional free sub-tags.

Purposes are first-class entries stored in `purposes/` as `<tag>.md` files. Each purpose has a description that defines what content belongs to it. On ingest:

1. Content is split into paragraphs (`\n\n`).
2. Each paragraph is embedded via OpenAI `text-embedding-3-small` and matched (cosine similarity) against all purposes' embeddings.
3. Top-1 purpose above `WIKI_SIMILARITY_THRESHOLD` wins; below threshold falls into auto-created `general` purpose.
4. Consecutive same-purpose paragraphs are grouped into chunks.
5. If only one chunk results → single doc tagged with that purpose.
6. If multiple chunks → parent "container" doc + child chunk docs, linked via `PartOf` reasons. Each child is tagged with its own purpose.

Cross-purpose questions (e.g. "how does topic A interact with topic B?") are answered by searching across both purpose tags and following `PartOf` reasons back to parents.

## Data Model

### Type tags (unique per doc)

- `#thought` — raw fact / source data, in `thoughts/`
- `#entity` — recurring concept, in `entities/`
- `#reason` — directed edge between nodes, in `reasons/`
- `#question` — open question, in `questions/`
- `#conclusion` — synthesized knowledge, in `conclusions/`

### Purpose tag (unique per doc, lives in `purposes/`)

- `#<purpose-tag>` — user-defined topical bucket. `#general` is the auto-created fallback.

### Reason kinds

`supports | contradicts | extends | requires | references | derives | instances | PartOf`

`PartOf` is auto-generated when ingest creates child chunks under a parent.

## Vault Structure

```
.wiki/
  purposes/
    <tag>.md          — Purpose definition (frontmatter: id, tag, title) + description body
    <tag>.vec         — Cached f32 embedding (1536-dim, le bytes)
  thoughts/           — Thought documents
  entities/           — Entity documents
  reasons/            — Reason documents
  questions/          — Question documents
  conclusions/        — Conclusion documents
  ingest_log/         — Audit log of all ingests
  auto_links/         — Pending auto-link approvals
  assets/             — Extracted images, PDFs, etc.
  .search/            — Tantivy full-text index
  .obsidian/          — Obsidian config
```

Each document is Markdown with YAML frontmatter:

```yaml
---
id: <uuid>
title: <title>
tags: [<type-tag>, <purpose-tag>, ...subtags]
purpose: <purpose-tag>      # optional, redundant with tags but explicit
source_doc_id: <uuid>       # only on chunk children
created_at: <rfc3339>
updated_at: <rfc3339>
---

<body>
```

## Tools

### Purpose management

- `list_purposes()`
- `create_purpose({tag, title, description})`
- `delete_purpose({tag})` — does not delete tagged docs
- `reembed_purposes()` — force-rebuild all OpenAI embeddings

### Ingest

All ingest tools auto-classify via OpenAI embeddings. Pass `purpose_hint` to skip classification.

- `ingest_thought({title, content, purpose_hint?})`
- `ingest_entity({title, content, purpose_hint?})`
- `ingest_reason({from_id, to_id, kind, body, purpose_hint?})`
- `ingest_question({body, purpose_hint?})`
- `ingest_conclusion({title, body, purpose_hint?})`

### Read

- `get({doc_type, id})`
- `list({doc_type})`
- `list_open_questions()`
- `list_ingest_log()`

### Search

- `search_fulltext({query})` — tantivy
- `search_by_tag({tag})` — searches type tag, purpose tag, or sub-tag
- `search_reasons_for({node_id, direction})` — `from|to|both`

### Modify

- `update({doc_type, id, content?, tags?})`
- `delete_doc({doc_type, id})`
- `mark_question({question_id, status})` — `resolved|unanswerable|partial_answer`

### Workflow helpers

- `find_answers({question_id})` — fulltext search candidates
- `suggest_conclusion({entity_id})` — gates synthesis on graph signals

### Extract

- `extract_pdfs({paths})`
- `extract_youtube({ids})`

### Code (split index)

`code_index | code_open | code_read_body | code_search | code_list_bodies | code_find_large | code_list_languages | code_ref_graph | code_outline | code_validate | code_fn_tree`

## Environment

- `WIKI_PATH` — vault root (default `./.wiki`)
- `OPENAI_API_KEY` — required for ingest classification + reembed
- `WIKI_SIMILARITY_THRESHOLD` — cosine threshold (default `0.35`); below → `general`

## Search

- Full-text: tantivy index at `.wiki/.search/`
- Tag-based: frontmatter scan
- Embedding: only used for purpose classification, not retrieval
