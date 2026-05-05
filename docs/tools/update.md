# update

Batch-only. Edit many docs in one call. Wrap every payload in `{items: [...]}`, even for a single edit. Re-embeds and re-links the body when `content` is provided.

## Params

| Name | Type | Default | Description |
|---|---|---|---|
| `items` | [UpdateItem] | (required) | One per doc. |

### UpdateItem

| Name | Type | Default | Description |
|---|---|---|---|
| `doc_type` | string | (required) | `thoughts`/`entities`/`questions`/`conclusions`/`reasons`. |
| `id` | string | (required) | Doc id. |
| `content` | string | (optional) | Replacement body. Triggers re-embed + re-link. |
| `tags` | [string] | (optional) | Replacement tag set (full overwrite). |

## Returns

```json
{
  "count": 1,
  "ok": 1,
  "errors": 0,
  "results": [
    {
      "index": 0,
      "ok": true,
      "value": {
        "id": "...",
        "doc_type": "thoughts",
        "title": "...",
        "content": "...",
        "tags": [...],
        "auto_linked": [...]
      }
    }
  ]
}
```

`auto_linked` only present when `content` was supplied and auto-invariants are enabled.

## Examples

```json
{ "items": [
  { "doc_type": "thoughts", "id": "abc", "content": "Updated body with new [[entity]]." },
  { "doc_type": "questions", "id": "abc", "tags": ["learning", "answered"] }
] }
```

## Notes

- `doc_type` is **required** — no auto-resolve.
- Re-embed → re-link runs server-side when `content` is set.
- For bulk edge edits, use `ingest({items: [{kind: "reason", ...}, ...]})`.
- Items processed sequentially; per-item failures do not abort the batch.
