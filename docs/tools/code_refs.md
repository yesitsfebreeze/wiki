# code_refs

Walk the call graph for a symbol. Returns callers, callees, or both, to a configurable depth.

## Params

| Name | Type | Default | Description |
|---|---|---|---|
| `symbol` | string | (required) | Fully-qualified symbol. |
| `direction` | enum | `"both"` | `"callers"`, `"callees"`, `"both"`. |
| `depth` | int | 1 | Walk depth. |

## Returns

```json
{
  "symbol": "auto_link",
  "callers": [{ "symbol": "ingest", "path": "src/tools.rs", "loc": 88, "depth": 1 }],
  "callees": [{ "symbol": "embed", "path": "src/store.rs", "loc": 120, "depth": 1 }]
}
```

## Examples

```json
{ "symbol": "auto_link", "direction": "callers" }
```
```json
{ "symbol": "ingest", "direction": "both", "depth": 2 }
```

## Notes

- Replaces `code_fn_tree` and `code_ref_graph`.
- Edges built from the same code index `code_search` uses; lazy reindex applies.
- Depth > 3 is rarely useful and slow on large codebases.
