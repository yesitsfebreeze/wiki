# search

Batch-only. Run many queries in one call. Wrap every payload in `{items: [...]}`, even for a single query. Each item returns hits with full bodies, reasons, and 1-hop edges inline so most workflows complete in one call. Conclusion-suggestion banner surfaces automatically when an entity has accumulated enough converging evidence.

## Params

| Name | Type | Default | Description |
|---|---|---|---|
| `items` | [SearchItem] | (required) | One per query. |

### SearchItem

| Name | Type | Default | Description |
|---|---|---|---|
| `query` | string | (required) | Natural-language query, tag, or FTS expression depending on `mode`. |
| `mode` | enum | `"smart"` | `"smart"` (hybrid conclusion-first), `"fts"` (raw BM25), `"tag"` (exact tag match), `"qa"` (filter to question→conclusion edges), `"list"` (paginate all docs of a given type). |
| `doc_type` | string | — | Required when `mode="list"`. |
| `k` | int | 10 | Max hits returned. |
| `include_bodies` | bool | `true` | Include full body in each hit. |
| `include_reasons` | bool | `true` | Include reason edges in each hit. |
| `edges_depth` | int | 1 | Edge-walk depth (`0` to skip). |
| `raise_on_miss` | bool | `false` | When `mode=smart`/`qa` returns 0 hits, auto-raise the query as an open question. Off by default — opt in to avoid polluting the question pool with search artifacts. |

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
        "query": "...",
        "mode": "smart",
        "hits": [ ... ],
        "suggested_conclusions": [ ... ],
        "raised_question_id": "..."
      }
    }
  ]
}
```

`suggested_conclusions` only present when an entity hits the converging-reasons threshold. `raised_question_id` only present when `raise_on_miss=true` and the query missed.

## Examples

```json
{ "items": [
  { "query": "how does auto-link decide threshold?", "mode": "smart", "k": 5 },
  { "query": "purpose:learning", "mode": "tag", "k": 20, "include_bodies": false }
] }
```

## Notes

- Items processed sequentially.
- `mode="qa"` filters to questions and conclusions only.
- For deeper traversal of a single doc, use `get` with `depth > 1`.
