# list

Paginate all docs of a given type. Use to browse the vault by doc kind when you don't have a specific query.

## Params

| Name | Type | Default | Description |
|---|---|---|---|
| `doc_type` | string | (required) | One of: `thoughts`, `entities`, `questions`, `conclusions`, `reasons`. |
| `limit` | int | 20 | Max results per page. |
| `cursor` | int | 0 | Offset for pagination. Pass the `next_cursor` from the previous response. |

## Returns

```json
{
  "items": [{ "id": "...", "title": "...", "tags": [...], "purpose": "..." }],
  "next_cursor": 20,
  "total": 142
}
```

## Notes

- Returns doc previews only (no body). Use `get` to fetch the full body of a specific doc.
- For filtered listing by tag or semantic query, prefer `search mode=tag` or `search mode=smart`.
- `reasons` lists all edge docs — usually high volume; use a small `limit` or filter downstream.
