# Wiki Flows

Single Obsidian vault at `.wiki/`. Topical separation by **purpose tags**, not folders. Each doc: 1 type tag + 1 purpose tag.

See root `overview.md` for the canonical version. This is a mirror for in-vault discoverability.

---

## 1. Ingest (raw → graph)

```
ingest (raw data) → purpose gate → thoughts → entities → questions → conclusions → reasons
```

Ingest does NOT raise questions (drift prevention). Question raising lives in the `/learn` pass and search-miss path.

---

## 2. Learn (graph → connected graph)

Random-walk sensemaker. Sample N docs (weighted by inverse edge degree — orphans first), then per doc:

1. **link & dedupe** — wikilink rewrite + paragraph-cosine fold into entities (Consolidates).
2. **connect** — query top-K (default 5) semantic neighbors, LLM classify edge kind (Supports/Contradicts/Extends/Requires/References/Derives/Instances/PartOf), create edges ≥ `cfg.edge_threshold` (default 0.7).
3. **raise** — only when `cfg.raise_questions=true` (off by default). LLM extracts ≤3 questions, template + semantic dedupe, purpose-cap backpressure.
4. **interrogate** — for open questions touching the doc, score answers (≥0.8 → Answers + resolved; 0.3..0.8 → Supports).
5. **promote** — resolved Q with strong Answers edge → conclusion (with merge-into-existing if cosine ≥ 0.92).

**Invariant:** every run adds ≥1 edge or question or conclusion, else logs `invariant_violated: true`.

Output dump: `.wiki/ingest_log/learn-<ts>.json`.

### PassConfig knobs

| Field | Default |
|-------|---------|
| `answer_threshold` | 0.8 |
| `support_threshold` | 0.3 |
| `edge_threshold` | 0.7 |
| `connect_k` | 5 |
| `raise_questions` | false |
| `qa_max_per_pass` | 50 |
| `conclusion_merge_threshold` | 0.92 |

---

## 3. Search (query → conclusions-first traversal)

```
query → [1] FTS over conclusions → top-k entry points
      → [2] walk reasons depth-1, fanout-5
      → [3] zero hits → hybrid fallback (BM25+cosine+RRF+MMR) + search-miss raise
      → [4] return conclusions + supporting docs + edge labels
```

---

## Loop

```
ingest → learn → search → (insight) → ingest → ...
```
