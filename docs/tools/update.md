# update

Edit an existing doc. Re-embeds, re-links, and invalidates stale edges automatically — no separate reindex call.

## Params

| Name | Type | Default | Description |
|---|---|---|---|
| `id` | string | (required) | Doc id. |
| `body` | string | (optional) | Replacement body. Triggers re-embed + re-link. |
| `title` | string | (optional) | Replacement title. |
| `tags` | [string] | (optional) | Replacement tag set (full overwrite). |
| `edges` | [object] | (optional) | Manual edge set. `[{from, to, kind}]`. Use to override an incorrect `auto_linked` edge from `ingest`. |

## Returns

```json
{
  "id": "...",
  "reembedded": true,
  "edges_added": [...],
  "edges_removed": [...]
}
```

## Examples

```json
{ "id": "thoughts/learning/abc", "body": "Updated body with new [[entity]]." }
```
```json
{ "id": "thoughts/learning/abc", "edges": [{ "from": "thoughts/learning/abc", "to": "entities/learning/x", "kind": "Consolidates" }] }
```

## Notes

- Re-embed → re-link → invalidate stale edges runs server-side; no separate `reembed_purposes` MCP call (that moved to CLI).
- Manual edge override is the supported escape hatch for bad auto-links from `ingest`.
