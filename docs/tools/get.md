# get

Batch-only. Fetch many docs in one call, each with reasons + 1-hop neighbors. Wrap every payload in `{items: [...]}`, even for a single fetch.

## Params

| Name | Type | Default | Description |
|---|---|---|---|
| `items` | [GetItem] | (required) | One per doc to fetch. |

### GetItem

| Name | Type | Default | Description |
|---|---|---|---|
| `id` | string | (required) | Doc id. |
| `doc_type` | string | (auto) | `thoughts`/`entities`/`questions`/`conclusions`/`reasons`. Auto-detected when omitted. |
| `depth` | int | 1 | Edge-walk depth. `1` = direct neighbors, `2` = neighbors-of-neighbors. |

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
        "doc": { ... },
        "reasons": [ ... ],
        "inbound": [ ... ],
        "outbound": [ ... ],
        "edges_by_depth": { "1": [...], "2": [...] }
      }
    }
  ]
}
```

## Examples

```json
{ "items": [
  { "id": "abc-123" },
  { "id": "def-456", "depth": 2 }
] }
```

## Notes

- `doc_type` auto-detection scans all five doc types. Pass it explicitly to skip lookup when you know the type.
- `depth=0` returns only the doc + its direct reasons.
- `inbound` = reason docs whose `to_id` is this doc (answers "what wikilinks INTO X?"). `outbound` = reason docs whose `from_id` is this doc. `reasons` keeps both for back-compat.
- For full removal use `delete_doc`; to change a body use `update`.
