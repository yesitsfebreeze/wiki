---
name: learn
description: Wiki sensemaker. /learn → densify pass. /learn <question> or "drive Q&A" → research-to-thoughts flow. Triggers `learn_pass` MCP tool. Always reads docs first.
tags:
  - skill
  - wiki
---

# Learn

**First action (mandatory):** `docs({name: "learn"})` — read fully before any write.

## Densify

```
learn_pass({force: true, raise_questions: true})
```

## Answer questions

All multi-doc tools (`ingest`, `mark_question`, `search`, `get`, `update`) are **batch-only** — wrap every payload in `{items: [...]}`, even for a single record.

1. `search({items: [{query, mode: "qa", include_bodies: true, include_reasons: true, edges_depth: 1}]})` — check `suggested_conclusions` per result.
2. Research → bundle all claims into one call:
   ```
   ingest({items: [
     {kind: "thought", body: "[[<question_id>]] ...claim 1..."},
     {kind: "thought", body: "[[<question_id>]] ...claim 2..."}
   ]})
   ```
   Body-start `[[<question_id>]]` mints a `Supports` edge — evidence stacks across thoughts.
3. `learn_pass({force: true, raise_questions: false})` — promotes a conclusion + hard-deletes the question (lifecycle is open|graveyard|deleted) once `support_promote_floor` (default 3) Supports edges accumulate or one candidate clears `answer_threshold` (default 0.6).
4. Still open after the pass? Either lower thresholds (`learn_pass({force:true, support_threshold:0.2, support_promote_floor:1})`) or fall back to `mark_question({items: [{question_id, status: "deleted"}]})` for already-answered questions, or `status: "buried"` to park unanswerable ones in `questions/graveyard/`.
