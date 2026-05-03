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

The MCP tool surface was consolidated to ~13 tools. Most loops now finish in 1–2 calls because invariants (auto-link, auto-mark-answered, auto-suggest-conclusion, lazy reindex) run server-side. Read `docs/concepts/ingest_flow.md` for vocab; read `docs/tools/<name>.md` for any tool you call.

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
ingest source → ingest({type:"thought", body})    [auto: classify, embed, link, wikilink-resolve]
              → (questions surface as open via background learn pass)
              → search({mode:"qa"}) when ready to answer        [returns suggested_conclusions]
              → ingest({type:"conclusion", body})  [auto: marks matching open question answered]

query → search({query, include:{bodies:true, reasons:true, edges_depth:1}})
        (conclusions-first inside server; one call returns hits + bodies + edges + reasons)
```

## Modes

| Mode | Trigger | Action |
|------|---------|--------|
| **ingest** | files in `.wiki/ingest/` | one `ingest({type:"thought", body})` per atomic claim. |
| **query** | substantive question or pre-conclusion synthesis | `search({query, mode:"smart"})` — single call returns hits, bodies, reasons, edges, and any `suggested_conclusions` banner. |
| **answer** | open question with enough support | `ingest({type:"conclusion", body})` — auto-marks the matching question. |
| **health** | periodic | `list_open_questions`, scan ingest responses for `auto_linked` precision. |

Background processes run automatically (no MCP call needed):
- **Auto-`learn`**: triggers on N=20 ingests OR M=10min idle. Runs link/dedupe + **connect** (top-K neighbor edge classification) + Q&A pass. Question raising is opt-in via `raise_questions: true` (off by default to keep ingest-driven passes quiet).
- **Auto-`code_index`**: filesystem watcher; lazy reindex on `code_search` if stale.
- **Auto-conclusion-suggestion**: surfaced inside `search` response, not a separate tool.

Force a learn pass via MCP: `learn_pass({force: true, raise_questions: true})`. Purpose admin via MCP: `create_purpose`, `delete_purpose`, `list_purposes`, `reembed_purposes`.

The learn pass is a **random-walk sensemaker** — samples docs weighted by inverse edge degree (orphans first), forges typed edges between semantic neighbors, raises questions, answers questions, promotes resolved questions to conclusions. Densification depends on the connect step, not just the question-answering loop. See `overview.md` step 2 for the full algorithm.

## Ingest

When user adds files to `.wiki/ingest/`:

1. For each plain-text source, extract atomic claims.
2. **One call per claim**: `ingest({type:"thought", body})`. Server auto-classifies purpose, embeds, scans nearest neighbors within the purpose cluster, creates edges (cosine ≥ 0.82, top-5 cap), and resolves any `[[wikilinks]]` in the body.
3. For recurring concepts: `ingest({type:"entity", body})`.
4. Audit `auto_linked` in the response. To override a wrong edge: `update({id, edges:[...]})` or `delete_doc` on the bad reason.
5. Delete the source from `.wiki/ingest/` after success.

`link_doc` is gone — auto-link is built into every `ingest`.

## Q&A loop

Old loop: `find_answers` → `ingest_conclusion` → `mark_question` (3 calls). New loop:

1. `list_open_questions({purpose})` — pick a question worth answering.
2. `search({query: question_body, mode:"qa", include:{bodies:true, reasons:true, edges_depth:1}})` — one call returns evidence; check the `suggested_conclusions` banner for a server-suggested synthesis.
3. `ingest({type:"conclusion", body: synthesis})` — server scans open questions; if cosine match ≥ threshold, the matching question is auto-marked `answered` and an `Answers` edge is created. Check `promoted: {question_id, marked: "answered"}` in the response.

`mark_question` only needed for manual overrides (e.g. dropping a stale question).

## Query — `search` conclusions-first

`search({query, mode:"smart", k:5})` returns hits with full `body`, inline `reasons`, and depth-1 `edges` already attached. No follow-up `get` or `search_reasons_for` needed for shallow reads. For deeper traversal: `get({id, depth:2})`.

```
1. search({query, mode:"smart"}) — server runs hybrid search, prefers conclusions as entry points,
   walks reasons, returns hits + bodies + edges + reasons inline.
2. If shallow read insufficient → get({id, depth:2}) on the most relevant hit.
3. If zero conclusion hits → server falls back to full-vault hybrid (BM25+cosine+RRF+MMR) automatically.
```

Cite all walked IDs in answers. If the answer adds durable insight → `ingest({type:"conclusion", body})`.

## Tool reference (final surface, ~13 tools)

### Query
- `search({query, mode?, k?, include?})` — primary read path. Modes: `smart` (default), `fts`, `tag`, `qa`. Returns bodies + reasons + edges + `suggested_conclusions`.
- `get({id, depth?})` — single doc with reasons + 1-hop edges always; `depth>1` for deeper walks.

### Write
- `ingest({type, body, tags?, refs?})` — all 5 doc types. Auto-embeds, auto-links, auto-resolves `[[wikilinks]]`, auto-answers matching questions when `type=conclusion`.
- `mark_question({id, state})` — manual override only.
- `update({id, body?, title?, tags?, edges?})` — auto re-embeds + re-links.
- `delete_doc({id})` — cascades edge cleanup.

### Code
- `code_search({query, kind?, lang?, k?})` — symbol/regex/semantic. Lazy reindex.
- `code_read({path?|symbol?, granularity})` — `outline | file | fn`.
- `code_refs({symbol, direction?, depth?})` — `callers | callees | both`.

### Meta
- `list_open_questions({purpose?, k?})` — gap-finding entry point.
- `docs({name?})` — fetch this surface's own documentation.

### Admin (MCP-exposed; no CLI)
`list`, `list_purposes`, `create_purpose`, `delete_purpose`, `reembed_purposes`, `code_index`, `code_validate`, `learn_pass`, `learn_from_feedback`, `migrate_templated_questions`, `recompute_weights`, `sanitize`. Everything is on the MCP surface — the `wiki` binary itself only exposes Claude Code hook subprocesses (`hook`, `stop-hook`, `code-read-hook`).

### Replaced (do not call — stale tool names)
| Old | New |
|---|---|
| `query`, `search_fulltext`, `search_by_tag`, `find_answers`, `search_reasons_for`, `suggest_conclusion` | `search` |
| `ingest_thought`, `ingest_entity`, `ingest_question`, `ingest_reason`, `ingest_conclusion`, `link_doc` | `ingest` |
| `code_open`, `code_read_body`, `code_outline` | `code_read` |
| `code_fn_tree`, `code_ref_graph` | `code_refs` |

## Aliases

Entity frontmatter:
```yaml
aliases:
  - Alt Name
  - Acronym
```
Linker matches title + aliases. ≥3 chars to avoid stop-word noise.

## Environment

- `WIKI_PATH` — vault root (default `./.wiki`)
- `OPENAI_API_KEY` — required for ingest classification, dedupe, Q&A LLM calls
- `WIKI_SIMILARITY_THRESHOLD` — purpose classification cosine (default `0.35`)
- `WIKI_DEDUPE_THRESHOLD` — paragraph dedupe cosine (default `0.85`)
- `WIKI_ALIAS_THRESHOLD` — entity alias merge cosine (default `0.92`)
- `WIKI_AUTO_INVARIANTS` — set `1` to enable server-side auto-link/auto-answer (default on after Phase B)

Per-call tunables: `auto_link_threshold` (default `0.82`), `auto_link_cap` (default `5`), `answer_threshold` (default `0.8`).

## Guardrails

- `.wiki/ingest/` is an inbox — drains on successful ingest.
- No `store` parameter — single store.
- Reason filenames deterministic (`<from>-<kind>-<to>`) — never rename.
- One purpose per doc; multi-purpose auto-splits during ingest.
- Contradictions visible — `Contradicts` reason auto-created if scan finds opposing claims; prior claim retained.
- No entities without supporting thoughts.
- Conclusions ideally come from the Q&A loop (matching open question), not freehand. Freehand `ingest({type:"conclusion"})` works but skips the auto-answer link.
- Self-link prevented.
- Skip code fences, inline backticks, existing `[[...]]`, markdown links during wikilink rewrite.
- Paragraphs <40 chars not embedded.
- Always audit `auto_linked` in ingest responses — false positives are tunable but not zero.
- After MCP signature changes, restart MCP server/client session.

## When to run

- After every `/ingest` batch — ingest each claim then trust the auto-`learn` background pass.
- Before substantive query — `search` (read-only).
- When user reports "wiki feels disconnected", "too much duplication", "missing answers", or "no conclusions": run `wiki learn --force` from CLI.

## Anti-patterns

- Calling old tool names (`query`, `ingest_thought`, `link_doc`, `find_answers`, `search_reasons_for`, etc.) — they are removed/deprecated. Use the new surface.
- Calling `get` after `search` for the body — `search` already returns bodies inline.
- Calling `mark_question` after `ingest({type:"conclusion"})` — it is auto-marked; check `promoted` in the response.
- Running freehand `ingest({type:"conclusion"})` without first checking `suggested_conclusions` from `search` → orphan conclusions.
- Calling the `wiki` binary with anything other than `hook|stop-hook|code-read-hook` — all wiki ops are MCP tools now.
