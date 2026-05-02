# Wiki Ingest Flow

Read this before using any ingest tools. The wiki is a single Obsidian vault at `.wiki/`. Topical separation is done via **purpose tags**, not separate stores. Each doc gets exactly one type tag + one purpose tag (auto-classified via OpenAI embeddings).

## The Flow

```
Raw Document
    ↓
[1] Create Thoughts (raw atomic facts) — auto-chunked by purpose
    ↓
[2] Search for similar thoughts (3+ similar = entity candidate)
    ↓
[3] Create Entity (consolidate related thoughts) — auto-classified
    ↓
[4] Create Reasons (links between thoughts/entities/questions)
    ↓
[5] Ask questions (unkowns)
    ↓
[5] Synthesize Conclusions (from related nodes + reasons)
    ↓
[6] Learn pass — wikilink entity mentions, fold duplicate paragraphs into canonical entity, emit `Consolidates` reasons. See `.claude/skills/learn/SKILL.md`.
```

## Purposes

Purposes live in `.wiki/purposes/<tag>.md`. Each has a description that defines what content belongs there. On ingest, content is split on `\n\n`, each paragraph is embedded via OpenAI `text-embedding-3-small`, and each is matched (cosine) against all purposes. Top-1 above `WIKI_SIMILARITY_THRESHOLD` (default `0.35`) wins; below threshold falls into auto-created `general`.

If a single document spans multiple purposes:
- Multiple consecutive paragraphs of the same purpose collapse into one chunk.
- Each chunk becomes a child doc (own purpose tag).
- A parent "container" doc holds the original full content.
- Each child is linked to the parent via a `PartOf` reason.

To skip classification, pass `purpose_hint`.

### Purpose tools

- `list_purposes()` — show all
- `create_purpose({tag, title, description})` — define a new topical bucket
- `delete_purpose({tag})` — does not delete tagged docs
- `reembed_purposes()` — force-rebuild OpenAI cache (`<tag>.vec` sidecars)

## Step 1: Thoughts (raw data)

**What:** Extract atomic facts from a source document. One thought = one claim.

**Tool:** `ingest_thought({title, content, purpose_hint?})`

**Behavior:**
- Auto-chunks `content` by purpose. If multi-purpose → parent + children + `PartOf` reasons.
- Returns either a single doc or `{parent, chunks, chunk_count}`.

**Example:** From "Python is a programming language created by Guido van Rossum in 1991":
- `ingest_thought({title: "Python language origin", content: "Python is a programming language created by Guido van Rossum in 1991"})`

## Step 2: Similarity check (manual — no auto-suggest yet)

After ingesting thoughts, periodically:
- `search_fulltext({query: "<topic>"})` to find clusters of similar thoughts
- If 3+ thoughts cover the same concept → promote to an entity

## Step 3: Entities (consolidated concepts)

**What:** A concept that multiple thoughts reference.

**Tool:** `ingest_entity({title, content, purpose_hint?})`

Same chunking rules as thoughts. Use `purpose_hint` when the entity belongs entirely to one bucket (saves API calls).

## Step 4: Reasons (knowledge graph edges)

**Kinds:** `supports | contradicts | extends | requires | references | derives | instances | PartOf | Answers | Consolidates`

**Tool:** `ingest_reason({from_id, to_id, kind, body, purpose_hint?})`

`PartOf` is auto-generated when ingest produces chunked children — do not create manually.

After ingesting an entity, manually link it to contributing thoughts:
```
ingest_reason({from_id: <entity_id>, to_id: <thought_id>, kind: "Consolidates", body: "..."})
```

## Step 5: Conclusions

**Tool:** `ingest_conclusion({title, body, purpose_hint?})`

Only create when an entity has linked reasons + ≥2 resolved questions. Use `suggest_conclusion({entity_id})` to gate.

---

# Search & Learning

## Answering open questions

1. `list_open_questions()` — all questions not marked resolved
2. `find_answers({question_id})` — fulltext-search candidates with suggested reason kinds
3. For each candidate that answers: `ingest_reason({from_id: question_id, to_id: candidate_id, kind: "Answers", body})`
4. `mark_question({question_id, status: "resolved"|"unanswerable"|"partial_answer"})`

## Cross-purpose queries

Single store enables cross-topic search:
- `search_by_tag({tag: "<purpose>"})` — all docs in one purpose bucket
- `search_fulltext({query: "..."})` — across everything; results span purposes
- Follow `PartOf` reasons via `search_reasons_for({node_id, direction: "to"})` to walk from a chunk back to its parent multi-purpose doc

## Learning loop

```
ingest_thought() / ingest_entity()
     ↓ (chunked by purpose, auto-tagged)
list_open_questions()
     ↓
for each question:
     ├─ find_answers()
     ├─ ingest_reason(kind: "Answers"|"Supports")
     └─ mark_question(status: ...)
     ↓
suggest_conclusion({entity_id})
     ├─ checks: linked reasons + ≥2 resolved questions
     └─ if can_conclude: ingest_conclusion()
```

# Inbox flow

`.wiki/ingest/` is the inbox for raw source files. The `/ingest` command:
1. Reads each file
2. Calls appropriate `ingest_*` tools
3. Deletes source on success
4. Runs `/learn` (ingest-time mode) on the just-ingested doc IDs to wikilink + dedupe before the batch closes.

# Environment

- `WIKI_PATH` — vault root (default `./.wiki`)
- `OPENAI_API_KEY` — required for classification (else falls back to `general`)
- `WIKI_SIMILARITY_THRESHOLD` — cosine threshold (default `0.35`)

# Important

- Do **not** pass a `store` parameter — there are no stores anymore.
- Do **not** create entities from thin air — thoughts come first.
- Each doc gets exactly **one** purpose tag. Multi-purpose content is split into chunks, each with its own single purpose tag.
- Reason files have deterministic names (`<from>-<kind>-<to>`); do not rename.
- Purposes are first-class. Add new ones via `create_purpose()` when introducing a new research area.
