# code_read

Read a file, function, or outline from the code index. Single tool covering all three granularities.

## Params

| Name | Type | Default | Description |
|---|---|---|---|
| `path` | string | (one of path/symbol required) | File path. |
| `symbol` | string | (one of path/symbol required) | Fully-qualified symbol name (`module::fn`). |
| `granularity` | enum | `"fn"` if symbol else `"file"` | `"outline"` (signatures only), `"file"` (full file), `"fn"` (single function body). |

## Returns

```json
{
  "path": "src/store.rs",
  "symbol": "auto_link",
  "granularity": "fn",
  "lang": "rs",
  "loc": 42,
  "body": "fn auto_link(...) { ... }"
}
```

For `granularity="outline"`:
```json
{ "path": "src/store.rs", "outline": [{ "symbol": "auto_link", "loc": 42, "kind": "fn" }] }
```

## Examples

```json
{ "symbol": "auto_link", "granularity": "fn" }
```
```json
{ "path": "src/store.rs", "granularity": "outline" }
```
```json
{ "path": "src/store.rs", "granularity": "file" }
```

## Notes

- Replaces `code_open`, `code_read_body`, `code_outline`.
- `granularity="fn"` requires `symbol`. `granularity="outline"`/`"file"` require `path`.
- Bodies fetched from the indexed sidecar — no filesystem read at call time.
