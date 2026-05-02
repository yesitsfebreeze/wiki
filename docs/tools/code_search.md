# code_search

Search the code index by symbol, regex, or semantic query. Lazily reindexes stale roots before serving the query.

## Params

| Name | Type | Default | Description |
|---|---|---|---|
| `query` | string | (required) | Symbol name, regex, or natural-language query. |
| `kind` | enum | `"symbol"` | `"symbol"`, `"regex"`, `"semantic"`. |
| `lang` | string | (any) | Restrict to one language (`rs`, `ts`, `py`, ...). |
| `k` | int | 20 | Max hits. |

## Returns

```json
{
  "hits": [
    { "path": "src/store.rs", "symbol": "auto_link", "lang": "rs", "loc": 42, "snippet": "..." }
  ]
}
```

## Examples

```json
{ "query": "auto_link", "kind": "symbol" }
```
```json
{ "query": "fn .*ingest.*", "kind": "regex", "lang": "rs" }
```

## Notes

- Lazy reindex: filesystem mtime checked against index; stale roots reindexed before the query runs.
- For deeper inspection of a hit, use `code_read`. For caller/callee graphs, use `code_refs`.
- Bulk admin (full reindex, list languages, validate index) moved to CLI: `wiki code index|validate|languages`.
