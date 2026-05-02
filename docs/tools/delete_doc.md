# delete_doc

Remove a doc and cascade-clean its edges.

## Params

| Name | Type | Default | Description |
|---|---|---|---|
| `id` | string | (required) | Doc id to delete. |

## Returns

```json
{ "id": "...", "deleted": true, "edges_removed": 4 }
```

## Examples

```json
{ "id": "thoughts/learning/abc" }
```

## Notes

- Cascades: all reasons referencing this doc are removed.
- Use to retract a hallucinated or wrong ingest. To keep the doc but rewrite, use `update`.
