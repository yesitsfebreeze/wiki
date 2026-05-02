# ingest

Single write tool for all five doc types. Server-side invariants embed the body, auto-link to nearest neighbors, resolve `[[wikilinks]]`, and (for conclusions) auto-mark matching open questions as answered.

## Params

| Name | Type | Default | Description |
|---|---|---|---|
| `type` | enum | (required) | `"thought"`, `"entity"`, `"question"`, `"reason"`, `"conclusion"`. |
| `body` | string | (required) | Doc content. Wikilinks `[[target]]` are parsed and linked. |
| `tags` | [string] | `[]` | Extra tags. Purpose tag is auto-classified if not provided. |
| `refs` | [string] | `[]` | Explicit edges the caller knows about. Merged with auto-linked. |

## Returns

```json
{
  "id": "thoughts/learning/abc",
  "auto_linked": [
    { "id": "entities/learning/x", "score": 0.87, "reason": "Consolidates" }
  ],
  "promoted": { "question_id": "questions/learning/q-abc", "marked": "answered" }
}
```

`promoted` is present only when `type=conclusion` matches an open question, or when `type=question` matches an existing conclusion.

## Examples

```json
{ "type": "thought", "body": "Cosine threshold 0.82 chosen to balance precision and recall." }
```
```json
{ "type": "conclusion", "body": "Auto-link threshold should default to 0.82.", "tags": ["learning"] }
```
```json
{ "type": "reason", "body": "Supports", "refs": ["thoughts/a", "thoughts/b"] }
```

## Notes

- Replaces `ingest_thought`, `ingest_entity`, `ingest_question`, `ingest_reason`, `ingest_conclusion`, and `link_doc`.
- Server-side invariants (always run):
  1. Embed body.
  2. Nearest-neighbor scan within purpose cluster → top-k similar (cosine ≥ 0.82) become edges.
  3. Parse `[[wikilinks]]` → resolve + edge. Missing targets recorded as orphans, not errors.
  4. `type=conclusion`: scan open questions; match → auto `mark_question(answered)` + Answers edge.
  5. `type=question`: scan existing conclusions; match → auto-answer.
- `auto_linked` returned so caller can audit. To remove a bad auto-edge, call `update({id, edges: [...]})` or `delete_doc` on the reason node.
- Top-5 cap on auto-linked count per ingest.
