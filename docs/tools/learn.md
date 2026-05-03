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

## The lifecycle

```
raw source
  → ingest({type:"thought", body}) per atomic claim     [auto: classify, embed, auto-link]
  → (questions surface during background learn pass when raise_questions:true)
  → search({mode:"qa"})  [returns suggested_conclusions banner — server-bound matches]
  → if no banner: research → more thoughts (with [[question_id]] wikilink in body)
  → learn_pass({force:true})  [server promotes conclusion + marks question resolved]
```

## Binding rules (must follow)

1. **Never** `ingest({type:"conclusion"})` freehand. Conclusions are output of `learn_pass` over supporting thoughts. Direct ingest creates orphan synthesis with no evidence trail.
2. `refs` parameter creates **`References`** edges, **not `Answers`**. Do not use `refs` to "answer" a question.
3. To bind a thought to a question, put `[[<question_id>]]` (or the question's title) at the start of the thought body. Server resolves wikilinks → creates Supports/References edges.
4. Before answering an open question, **always** `search({query: question_body, mode: "qa", include: {bodies:true, reasons:true, edges_depth:1}})` first. Read the `suggested_conclusions` field — if non-empty, use the bound conclusion; skip new ingest.
5. Answer threshold for auto-mark is cosine ≥ 0.8 between conclusion and question bodies. Below that, the conclusion stays standalone with `References` edges. Manual fallback: `mark_question({id, state:"answered"})`.

## Q&A flow (driven by /learn or agent-led research)

For each open question:

1. `search({query: question_body, mode:"qa", include:{bodies:true, reasons:true, edges_depth:1}})`. Check `suggested_conclusions`.
2. If banner has a strong match → cite + stop.
3. Else: research the topic (web search, sources). Extract atomic claims.
4. For each claim: `ingest({type:"thought", body: "[[<question_id>]] ...claim text..."})`. Wikilink at start → server creates Supports edge thought → question.
5. After **all** thoughts ingested: `learn_pass({force:true, raise_questions:false})`. Server scans questions with supporting thoughts, synthesizes a conclusion via LLM over the supporting bodies, creates the conclusion doc, emits `Derives` edge question → conclusion + `Answers` edges, and tags the question `resolved`.
6. Verify: `get({id: question_id, depth:1})`. If still open after the pass: `mark_question({id, state:"answered"})` as final fallback.

## Densify-only flow

When user invokes `/learn` with no question target:

```
learn_pass({force:true, raise_questions:true})
```

Reports edges added, questions raised, conclusions promoted. The pass samples weighted by inverse edge degree (orphans first), forges typed edges between semantic neighbors, raises questions on linkable docs, runs cross-reference + cross-topic synthesis on open questions with supporting evidence.

## Environment

Per-call tunables: `auto_link_threshold` (default `0.82`), `auto_link_cap` (default `5`), `answer_threshold` (default `0.8`).

## Common mistakes

- Calling old tool names (`ingest_thought`, `find_answers`, `search_fulltext`, `link_doc`) — gone. Use `ingest`, `search`.
- `ingest({type:"conclusion"})` before ingesting supporting thoughts → orphan conclusion.
- Using `refs:[question_id]` expecting an Answers edge → only creates `References`.
- Skipping the `mode:"qa"` pre-step → agent re-derives an answer the server already has.
- Forgetting the wikilink in thought bodies → no Supports edge to the question → learn_pass cannot promote.
