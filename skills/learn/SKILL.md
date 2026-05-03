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

1. `search({query, mode: "qa", include: {bodies:true, reasons:true, edges_depth:1}})` — check `suggested_conclusions`.
2. Research → `ingest({type: "thought", body: "[[<question_id>]] ...claim..."})` per claim.
3. `learn_pass({force: true, raise_questions: false})`.
4. If still open: `mark_question({id, state: "answered"})`.
