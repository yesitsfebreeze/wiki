---
name: learn
description: Single entry point for the wiki — ingest raw sources, link/dedupe, raise questions, cross-reference, answer, promote resolved questions to conclusions, and query via conclusions-first traversal. Replaces the old `/wiki` skill. Use whenever ingesting, querying, or maintaining the persistent Obsidian-vault knowledge base.
tags:
  - skill
  - wiki
  - knowledge-base
  - linking
  - learning
---

# Learn — full wiki lifecycle

`/learn` is the **only** wiki skill. Covers ingest → link → raise questions → cross-reference → answer → promote → query. Single Obsidian vault at `.wiki/`. Topics separated by **purpose tags**, not folders.

Always read `.claude/tools/wiki/INGEST_FLOW.md` for vocab.

## Doc model

Each doc: 1 type tag + 1 purpose tag.

| Type | Role |
|------|------|
| `thought` | Atomic fact from a source |
| `entity` | Consolidated concept (linked to contributing thoughts) |
| `reason` | Directed edge between two docs (`Consolidates`, `Answers`, `Supports`, `Contradicts`, `Extends`, `Requires`, `References`, `Derives`, `Instances`, `PartOf`) |
| `question` | Open question raised by content |
| `conclusion` | Synthesized answer — primary query entry point |

## Lifecycle (the loop)

```
ingest source → ingest_thought (auto-chunked by purpose)
              → link_doc       (wikilink + paragraph dedupe)
              → raise          (LLM extracts open questions from new content)
              → cross_reference (search wiki for candidate answers)
              → answer         (if score ≥ 0.8: ingest_reason Answers, mark resolved)
              → promote        (resolved Q → conclusion + Derives/References edges)

query → smart_search (conclusions first → walk reasons → return tree)
```

## Modes

| Mode | Trigger | Action |
|------|---------|--------|
| **ingest** | files in `.wiki/ingest/` | drain inbox via `ingest_thought` per atomic claim, then run `learn_pass` on new docs |
| **batch learn** | `/learn [limit] [purpose]` | full pipeline (link + dedupe + raise + cross-ref + answer + promote) on docs |
| **query** | substantive question or pre-conclusion synthesis | `smart_search` conclusions-first, walk depth-1 fanout-5 |
| **health** | periodic | `list_purposes`, `list_open_questions`, drain `find_answers` queue |

## Ingest

When user adds files to `.wiki/ingest/`:

1. For each source, extract atomic claims.
2. `ingest_thought({title, content})` per claim. Auto-chunked by purpose; multi-purpose yields parent + children + `PartOf` reasons.
3. Recurring concepts → `ingest_entity({title, content})`.
4. Manual entity↔thought link: `ingest_reason({from_id, to_id, kind: "Consolidates", body})`.
5. Delete source from `.wiki/ingest/` after success.
6. Run `learn_pass({limit: <new_doc_count>})` to fold new docs into the graph.

Pass `purpose_hint` only when bucket certain (skips one OpenAI call).

## Learn pass — full pipeline

`learn_pass({limit, purpose, dry_run})` walks `thoughts` ∪ `conclusions` and runs each doc through:

### Step 1 — Link & dedupe (existing)

- Build entity index. Embed bodies once per pass.
- Per doc: regex-find entity titles + aliases, skip protected ranges (code fences, backticks, existing `[[...]]`, markdown links), rewrite as `[[entities/<purpose>/<slug>|<surface>]]`. One link per entity per doc.
- Paragraph dedupe: split on `\n\n`, embed paragraphs ≥40 chars, drop those ≥ `WIKI_DEDUPE_THRESHOLD` (default `0.85`) cosine vs an entity body, emit `Consolidates` reason.

### Step 2 — Raise questions (NEW)

- LLM extracts ≤3 open questions raised by the doc body.
- Dedup via `fnv_question_id(question)` against existing `questions/`.
- New ones: `ingest_question({body, purpose: doc.purpose})`, link `question →References→ source_doc`.

### Step 3 — Cross-reference (NEW)

- For each new (or still-open) question, run `smart_search(question, k=5)` against the vault.
- LLM scores candidates 0..1.

### Step 4 — Answer (NEW)

- Score ≥ 0.8 → `ingest_reason(question →Answers→ candidate)`, mark question `resolved`.
- 0.3..0.8 → `Supports`, leave open.
- < 0.3 → skip.

### Step 5 — Promote (NEW)

When a question becomes resolved:

- LLM synthesizes a 1-paragraph answer from the question + top Answers edges.
- `ingest_conclusion({title: question.title, body: synthesis, purpose: question.purpose})`.
- `ingest_reason(question →Derives→ conclusion)`.
- `ingest_reason(conclusion →References→ answer_doc)` per top edge.
- Idempotent: skip if conclusion w/ same `fnv_question_id` tag already exists.

## Query — `smart_search` conclusions-first

```
1. search_fulltext(query) restricted to type=conclusion → top-k entry points
2. for each conclusion:
     walk search_reasons_for(conclusion.id, "from") depth-1, fanout-5
     collect linked thoughts/entities/questions
3. if zero conclusion hits → fall back to full-vault hybrid search (BM25+cosine+RRF+MMR)
4. return: conclusions (primary) + supporting docs (context) + reason kinds
```

Cite all walked IDs in answers. If answer adds durable insight → `ingest_conclusion`.

## Inputs

| Param | Default | Purpose |
|-------|---------|---------|
| `limit` | 25 | Max docs per pass — bound OpenAI cost |
| `purpose` | `null` | Restrict to one purpose tag |
| `dry_run` | `false` | Log proposed edits + questions, no write |
| `qa` | `true` | Skip steps 2–5 if `false` (link+dedupe only) |

## Tool reference

### Purpose
- `list_purposes()` / `create_purpose` / `delete_purpose` / `reembed_purposes`

### Ingest (all accept optional `purpose_hint`)
- `ingest_thought` / `ingest_entity` / `ingest_reason` / `ingest_question` / `ingest_conclusion`

### Read
- `get({doc_type, id})` / `list({doc_type})` / `list_open_questions` / `list_ingest_log`

### Search
- `smart_search({question, tag?, k, top_n})` — **default for substantive queries**
- `search_fulltext({query})` — raw FTS sweep
- `search_by_tag({tag})`
- `search_reasons_for({node_id, direction})`

### Modify
- `update` / `delete_doc` / `mark_question`

### Workflow
- `learn_pass({limit, purpose, dry_run, qa})` — full pipeline
- `link_doc({doc_type, id, dry_run})` — single doc, link+dedupe only
- `find_answers({question_id})`
- `suggest_conclusion({entity_id})`

### Extract
- `extract_pdfs({paths})` / `extract_youtube({ids})`

## Aliases

Entity frontmatter:
```yaml
aliases:
  - Alt Name
  - Acronym
```
Linker matches title + aliases. ≥3 chars to avoid stop-word noise.

## Output schema (learn_pass)

```json
{
  "pass_id": "<rfc3339>",
  "docs_scanned": N,
  "docs_modified": M,
  "links_added": K,
  "paragraphs_merged": P,
  "questions_raised": Q,
  "questions_answered": A,
  "conclusions_promoted": C,
  "entity_count": E,
  "purpose_filter": "<tag|null>",
  "dry_run": false,
  "details": [...]
}
```

Dump at `.wiki/ingest_log/learn-<ts>.json`.

## Environment

- `WIKI_PATH` — vault root (default `./.wiki`)
- `OPENAI_API_KEY` — required for ingest classification, dedupe, Q&A LLM calls
- `WIKI_SIMILARITY_THRESHOLD` — purpose classification cosine (default `0.35`)
- `WIKI_DEDUPE_THRESHOLD` — paragraph dedupe cosine (default `0.85`)
- `WIKI_ALIAS_THRESHOLD` — entity alias merge cosine (default `0.92`)
- `WIKI_ANSWER_THRESHOLD` — Q&A answer-link cosine (default `0.8`)
- `WIKI_SUPPORT_THRESHOLD` — weak-link floor (default `0.3`)

## Guardrails

- `.wiki/ingest/` is an inbox — drains on successful ingest.
- No `store` parameter — single store.
- Reason filenames deterministic (`<from>-<kind>-<to>`) — never rename.
- One purpose per doc; multi-purpose auto-splits.
- Contradictions visible — emit `Contradicts` reason, keep prior claim.
- No entities without supporting thoughts.
- No conclusions without resolved-question Derives chain (auto-promote path) or explicit `suggest_conclusion` review.
- Self-link prevented.
- Skip code fences, inline backticks, existing `[[...]]`, markdown links during rewrite.
- Paragraphs <40 chars not embedded.
- `dry_run: true` first on a fresh vault to inspect proposed merges + questions.
- After MCP signature changes, restart MCP server/client session.

## When to run

- After every `/ingest` batch — `limit` = number of newly ingested docs.
- Periodic full-vault sweep — paginated by `limit`.
- Before substantive query — `smart_search` (read-only).
- When user reports "wiki feels disconnected", "too much duplication", "missing answers", or "no conclusions".

## Anti-patterns

- Querying via `search_fulltext` directly when `smart_search` would walk conclusion tree → shallow answers.
- Calling `ingest_conclusion` manually without a resolved question chain → orphan conclusion.
- Running `learn_pass` with `qa: false` permanently → wiki never grows the conclusion layer.
- Running without `dry_run` on first pass over fresh vault → noisy false-positive merges.
