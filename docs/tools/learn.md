# Wiki Learn Protocol

Read this **before** ingesting, querying, or driving Q&A. Single Obsidian vault at `.wiki/`. Topics separated by **purpose tags**, not folders.

## Doc model

| Type | Role |
|------|------|
| `thought` | Atomic claim from a source. Smallest unit of evidence. |
| `entity` | Consolidated concept linking ≥3 thoughts. |
| `reason` | Directed edge. Kinds: `Answers`, `Supports`, `Contradicts`, `Extends`, `Requires`, `References`, `Derives`, `Instances`, `PartOf`, `Consolidates`. |
| `question` | Open question. Resolves when an `Answers` edge to a conclusion exists or `mark_question` is called. |
| `conclusion` | Synthesized answer. Primary query entry point. **Server-promoted from supporting thoughts**, not freehand. |

**All multi-doc tools (`ingest`, `mark_question`, `search`, `get`, `update`) are batch-only. Wrap every payload in `{items: [...]}`, even for one record.** Bundle related writes/reads into a single call to avoid N+1 round-trips — wikilink resolution is sequential within a batch, so prior items are visible to later ones.

## The lifecycle

```
raw source
  → ingest({items: [{kind:"thought", body}, ...]}) per atomic claim   [auto: classify, embed, auto-link]
  → (questions surface during background learn pass when raise_questions:true)
  → search({items: [{query, mode:"qa"}]})  [returns suggested_conclusions banner — server-bound matches]
  → if no banner: research → more thoughts (with [[question_id]] wikilink in body)
  → learn_pass({force:true})  [server promotes conclusion + marks question resolved]
```

## Binding rules (must follow)

1. **Never** `ingest({items:[{kind:"conclusion", ...}]})` freehand. Conclusions are output of `learn_pass` over supporting thoughts. Direct ingest creates orphan synthesis with no evidence trail.
2. `refs` parameter creates **`References`** edges, **not `Answers`**. Do not use `refs` to "answer" a question.
3. To bind a thought to a question, put `[[<question_id>]]` (or the question's title) at the **start** of the thought body. Body-start wikilink to a question → `Supports` edge (evidence; multiple thoughts can stack). Body-start same-purpose non-question → `Supports`. Mid-body or cross-purpose → `References`. **No auto-mark** — `learn_pass` decides when accumulated `Supports` clear the floor and promotes a conclusion + marks the question answered. To force an immediate answer, ingest a reason explicitly: `ingest({items:[{kind:"reason", from_id:<thought>, to_id:<qid>, reason_kind:"Answers", body:"..."}]})`.
4. Before answering an open question, **always** `search({items:[{query: question_body, mode: "qa", include_bodies:true, include_reasons:true, edges_depth:1}]})` first. Read the `suggested_conclusions` field — if non-empty, use the bound conclusion; skip new ingest.
5. Answer threshold for auto-mark is cosine ≥ 0.8 between conclusion and question bodies. Below that, the conclusion stays standalone with `References` edges. Manual fallback: `mark_question({items:[{question_id, status:"answered"}]})`.

## Q&A flow (driven by /learn or agent-led research)

For each open question:

1. `search({items:[{query: question_body, mode:"qa", include_bodies:true, include_reasons:true, edges_depth:1}]})`. Check `suggested_conclusions`.
2. If banner has a strong match → cite + stop.
3. Else: research the topic (web search, sources). Extract atomic claims.
4. Bundle all claims into one ingest:
   ```
   ingest({items: [
     {kind:"thought", body: "[[<question_id>]] ...claim 1..."},
     {kind:"thought", body: "[[<question_id>]] ...claim 2..."}
   ]})
   ```
   Body-start wikilink to the question → server creates `Supports` edge thought → question. Multiple `Supports` accumulate; `learn_pass` synthesizes once `support_promote_floor` (default 3) is met or one candidate clears `answer_threshold` (default 0.6).
5. After **all** thoughts ingested: `learn_pass({force:true, raise_questions:false})`. Server scans questions with supporting thoughts, synthesizes a conclusion via LLM over the supporting bodies, creates the conclusion doc, emits `Derives` edge question → conclusion + `Answers` edges, and tags the question `resolved`.
6. Verify: `get({items:[{id: question_id, depth:1}]})`. If still open after the pass: `mark_question({items:[{question_id, status:"answered"}]})` as final fallback.

## Densify-only flow

When user invokes `/learn` with no question target:

```
learn_pass({force:true, raise_questions:true})
```

Reports edges added, questions raised, conclusions promoted. The pass samples weighted by inverse edge degree (orphans first), forges typed edges between semantic neighbors, raises questions on linkable docs, runs cross-reference + cross-topic synthesis on open questions with supporting evidence.

`limit` controls how many docs the pass scans (default `25`; pass `limit: 0` to scan the whole vault). The pass is bounded further by `qa_max_per_pass` LLM-call budget.

For deterministic pagination across a large vault, pass `start` (page offset). The universe is sorted by `(doc_type, id)` and the slice `[start, start + limit)` is processed. The response carries `next_start` (`null` once exhausted) and `total_universe`. Loop:
```
learn_pass({start: 0,  limit: 25})  // → next_start: 25
learn_pass({start: 25, limit: 25})  // → next_start: 50
...                                 // → next_start: null
```
Omit `start` to keep the legacy weighted-random sampling that prefers orphans for ad-hoc passes.

If the pass returns `invariant_violated: true`, read the `invariant_reason` field — it lists the active thresholds and skipped-recent count, the most common reason a real-looking pass produced no progress.

## Environment

Per-call tunables: `auto_link_threshold` (default `0.82`), `auto_link_cap` (default `5`), `answer_threshold` (default `0.6`), `support_threshold` (default `0.3`), `support_promote_floor` (default `3`, synthesis-floor — promote conclusion when N supports accumulate without a strong answer), `edge_threshold` (default `0.7`), `connect_k` (default `10`), `qa_max_per_pass` (default `50`), `conclusion_merge_threshold` (default `0.92`).

### Wikilink → References edges

`link_doc_internal` (run automatically by `learn_pass` and `ingest`) scans every `[[...]]` wikilink in a doc body and mints a `References` reason edge from the source doc to the target. Resolves three target forms:

- `[[<uuid>]]` — raw id, searched across all doc types.
- `[[<doc_type>/<id>]]` — direct lookup (e.g. `[[questions/<id>]]`).
- `[[entities/<purpose>/<slug>]]` — entity index slug lookup.

Edges inside fenced or inline code blocks are skipped. Re-running is idempotent: existing `References` edges from the source are not duplicated.

## Common mistakes

- Calling old tool names (`ingest_thought`, `find_answers`, `search_fulltext`, `link_doc`) — gone. Use `ingest`, `search`.
- Calling tools with the **old singleton shape** (`ingest({kind, body, ...})`) — gone. All multi-doc tools require `{items: [...]}`.
- `ingest({items:[{kind:"conclusion", ...}]})` before ingesting supporting thoughts → orphan conclusion.
- Using `refs:[question_id]` expecting an Answers edge → only creates `References`. Use a body-start `[[<question_id>]]` instead.
- Skipping the `mode:"qa"` pre-step → agent re-derives an answer the server already has.
- Forgetting the body-start wikilink in thought bodies → no `Supports` edge → learn_pass cannot promote.
- Putting the wikilink **mid-body** instead of at the start → only `References`, never `Supports`.
- Expecting one body-start wikilink to mark a question answered. It mints `Supports` only; promotion requires `learn_pass` to run.
