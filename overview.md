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

**Ingest does NOT raise questions.** Question raising is reserved for the `/learn` pass (LLM-driven) and search-miss path (idempotent, cheap). This prevents user-ingested content from spawning unwanted questions.

---

## 2. Learn (graph → connected graph)

**Random-walk sensemaker.** Each `/learn` densifies the graph by sampling docs and forging connections. Not driven by the open-questions queue — driven by random traversal across `thoughts ∪ conclusions` weighted by inverse degree (orphans first). Open questions are an *output*, not the input.

`learn_pass({limit, purpose, dry_run, qa, raise})`:

```
[0] count thoughts ∪ conclusions in scope (optionally filtered by purpose)
   ↓
[1] sample N docs via weighted-random selection
       weight(doc) = 1 / (1 + edge_degree(doc))
       (bias toward low-degree nodes — least-linked first; orphans surface fastest)
   ↓
[2] for each sampled doc:
       ├─ link & dedupe
       │     ├─ regex-find entity titles + aliases → rewrite as [[wikilinks]]
       │     └─ paragraph cosine ≥ WIKI_DEDUPE_THRESHOLD → fold into entity, emit Consolidates
       │
       ├─ connect  (graph densification — runs every doc, regardless of questions)
       │     ├─ query top-K (cfg.connect_k, default 5) semantic neighbors across whole vault
       │     ├─ LLM classify edge kind per neighbor:
       │     │     Supports / Contradicts / Extends / Requires /
       │     │     References / Derives / Instances / PartOf
       │     └─ create_reason for any edge ≥ cfg.edge_threshold (default 0.7) that doesn't already exist
       │
       ├─ raise  (only when cfg.raise_questions=true; off by default on ingest path)
       │     ├─ LLM extracts ≤3 open questions from doc
       │     ├─ template-shape filter (drop "How does X relate to similar concepts" etc.)
       │     ├─ semantic dedupe vs existing open questions in purpose
       │     └─ purpose-cap backpressure (open_questions_per_purpose_cap)
       │
       ├─ interrogate
       │     ├─ list open questions touching this doc (by tag, entity, or vector)
       │     ├─ try to answer each from doc + top-k neighbors
       │     │     ├─ score ≥0.8 → ingest_reason Answers, mark resolved
       │     │     ├─ 0.3..0.8 → Supports, leave open
       │     │     └─ <0.3 → skip
       │
       └─ promote
             └─ resolved Q with ≥1 strong Answers edge → ingest_conclusion + Derives + References
```

**Invariant:** every run must add ≥1 new edge OR new question OR conclusion. Zero-progress passes log a warning (`invariant_violated: true` in report) — widen N or lower threshold.

Run on every ingest batch and on demand. Iterative — each pass densifies more. Output dump: `.wiki/ingest_log/learn-<ts>.json` (sampled IDs, edges added, Qs raised, Qs resolved, conclusions promoted, invariant flag).

### Question-raising paths

| Path | Trigger | LLM? | Default |
|------|---------|------|---------|
| Ingest | new doc into vault | no | disabled (drift prevention) |
| Search-miss | `search` returns zero hits | no (query = title) | enabled |
| Learn pass | `/learn` with `qa=true, raise=true` | yes | opt-in via flag |

### PassConfig knobs

| Field | Default | Purpose |
|-------|---------|---------|
| `answer_threshold` | 0.8 | Cosine ≥ → `Answers` + mark resolved |
| `support_threshold` | 0.3 | Cosine ≥ but < answer → `Supports` |
| `edge_threshold` | 0.7 | Connect-step: emit typed edge ≥ this |
| `connect_k` | 5 | Connect-step: top-K neighbors per doc |
| `raise_questions` | false | Enable LLM question raising in pass |
| `qa_max_per_pass` | 50 | Hard LLM-call ceiling per pass |
| `conclusion_merge_threshold` | 0.92 | Merge into existing conclusion ≥ this |

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
                         → also raise question via search-miss path (idempotent)
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
