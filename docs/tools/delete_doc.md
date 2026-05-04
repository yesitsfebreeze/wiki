# delete_doc

Remove one or many docs of the same `doc_type`.

## Params

| Name | Type | Default | Description |
|---|---|---|---|
| `doc_type` | string | (required) | One of `thoughts`, `entities`, `conclusions`, `questions`, `reasons`. |
| `id` | string | — | Single doc id. Provide this OR `ids`. |
| `ids` | string[] | — | Batch list of doc ids. Provide this OR `id`. |

At least one of `id` / `ids` is required. Both may be provided; they are merged.

## Returns

```json
{
  "doc_type": "reasons",
  "deleted": 197,
  "failed": 0,
  "results": [
    { "id": "abc...", "deleted": true },
    { "id": "missing-id", "deleted": false, "error": "..." }
  ]
}
```

## Examples

Single delete:
```json
{ "doc_type": "thoughts", "id": "abc-123" }
```

Batch delete:
```json
{ "doc_type": "reasons", "ids": ["a", "b", "c"] }
```

## Notes

- Per-id failures do not abort the batch. Each result reports its own outcome.
- Index and cache are updated after each delete.
- Use `update` to keep a doc but rewrite its body.
