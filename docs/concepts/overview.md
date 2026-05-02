# Wiki Flows

Single Obsidian vault at `.wiki/`. Topical separation by **purpose tags**, not folders. Each doc: 1 type tag + 1 purpose tag.

---

## 1. Ingest (raw → graph)

```
ingest (raw data)
   ↓
purpose gate (cosine match against purpose descriptions; multi-purpose → parent + chunks + PartOf)
   ↓
thoughts (atomic claims, smallest body possible)
   ↓
entities (recurring concepts across 3+ thoughts)
   ↓
questions (open unknowns raised by content)
   ↓
conclusions (synthesized answers to resolved questions)
   ↓
reasons (typed edges between any nodes: Consolidates, Answers, Supports,
         Contradicts, Extends, Requires, References, Derives, Instances, PartOf)
```

Tools: `ingest_thought`, `ingest_entity`, `ingest_question`, `ingest_conclusion`, `ingest_reason`.
Inbox: `.wiki/ingest/` — `/ingest` drains it, then triggers learn pass on new doc IDs.

---

## 2. Learn (graph → connected graph)

`learn_pass({limit, purpose, dry_run, qa})` walks `thoughts ∪ conclusions` and folds each through:

```
new/unlinked doc
   ↓
[1] link & dedupe
       ├─ regex-find entity titles + aliases → rewrite as [[wikilinks]]
       └─ paragraph cosine ≥ WIKI_DEDUPE_THRESHOLD → fold into entity, emit Consolidates
   ↓
[2] raise questions (LLM extracts ≤3 open questions; dedup via fnv_question_id)
   ↓
[3] cross-reference (query top-5 candidates per open question)
   ↓
[4] answer (LLM scores 0..1)
       ├─ ≥0.8 → ingest_reason Answers, mark resolved
       ├─ 0.3..0.8 → Supports, leave open
       └─ <0.3 → skip
   ↓
[5] promote (resolved Q → ingest_conclusion + Derives + References per top edge)
```

Run after every ingest batch. Output dump: `.wiki/ingest_log/learn-<ts>.json`.

---

## 3. Search (query → conclusions-first traversal)

`query({question, tag?, k, top_n})`:

```
query
   ↓
[1] search_fulltext restricted to type=conclusion → top-k entry points
   ↓
[2] for each conclusion:
       walk search_reasons_for(conclusion.id, "from") depth-1, fanout-5
       collect linked thoughts/entities/questions
   ↓
[3] zero conclusion hits? → fall back to full-vault hybrid (BM25 + cosine + RRF + MMR)
   ↓
[4] return:
       ├─ conclusions (primary answer layer)
       ├─ supporting docs (context)
       └─ reason kinds (edge labels)
```

Cite all walked IDs in answers. Durable insight discovered during search → `ingest_conclusion` to grow the entry-point layer.

---

## Loop

```
ingest → learn → search → (insight) → ingest → ...
```

Conclusion layer is the primary query surface. Empty conclusion layer = shallow search. Run learn often.
