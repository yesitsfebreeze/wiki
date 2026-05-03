# search

Single retrieval entry point. Returns hits with full bodies, reasons, and 1-hop edges inline so most workflows complete in one call. Conclusion-suggestion banner surfaces automatically when a question has accumulated enough converging evidence.

## Params

| Name | Type | Default | Description |
|---|---|---|---|
| `query` | string | (required) | Natural-language query, tag, or FTS expression depending on `mode`. |
| `mode` | enum | `"smart"` | `"smart"` (hybrid conclusion-first), `"fts"` (raw BM25), `"tag"` (exact tag match), `"qa"` (filter to question→conclusion edges), `"list"` (paginate all docs of a given type). |
| `doc_type` | string | — | Required when `mode="list"`. One of: `thoughts\|entities\|questions\|conclusions\|reasons`. |
| `k` | int | 5 | Max hits returned. |
| `include` | object | `{bodies: true, reasons: true, edges_depth: 1}` | Toggle inline payload. Set `edges_depth: 0` to skip edges. |

## Returns

```json
{
  "hits": [
    {
      "id": "conclusions/learning/why-x",
      "type": "conclusion",
      "title": "Why X",
      "body": "...",
      "reasons": [{ "kind": "Derives", "from": "...", "to": "..." }],
      "edges": [{ "depth": 1, "id": "thoughts/...", "kind": "References" }]
    }
  ],
  "suggested_conclusions": [
    { "question_id": "questions/...", "candidate_body": "...", "support_count": 4 }
  ]
}
```

## Examples

```json
{ "query": "how does auto-link decide threshold?", "mode": "smart", "k": 5 }
```
```json
{ "query": "purpose:learning", "mode": "tag", "k": 20, "include": { "bodies": false } }
```

## Notes

- Replaces `query`, `search_fulltext`, `search_by_tag`, `find_answers`, `search_reasons_for`, `suggest_conclusion`, and the old `list` tool (via `mode="list"`).
- `mode="qa"` is the new equivalent of `find_answers`.
- `suggested_conclusions` only present when an open question hits the converging-reasons threshold; review then call `ingest({type:"conclusion", body})` to materialize.
- For deeper traversal of a single doc, use `get` with `depth > 1`.
