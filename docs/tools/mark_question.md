# mark_question

Manually transition a question's state. Mostly auto-driven by `ingest(type=conclusion)`; use this only when overriding.

## Params

| Name | Type | Default | Description |
|---|---|---|---|
| `id` | string | (required) | Question doc id. |
| `state` | enum | (required) | `"answered"`, `"dropped"`. |

## Returns

```json
{ "id": "questions/learning/q-abc", "state": "answered", "linked_conclusion": "conclusions/..." }
```

## Examples

```json
{ "id": "questions/learning/q-abc", "state": "dropped" }
```

## Notes

- When `state=answered` is set manually, the call attempts to resolve the most-similar conclusion and create an `Answers` edge.
- Auto-fired from `ingest(conclusion)` when cosine match ≥ threshold; check the ingest response's `promoted` field.
