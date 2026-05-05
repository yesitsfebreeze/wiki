# retag

Batch-only. Bulk-retag and bulk-purpose-move docs without touching `content`. Wrap every payload in `{items: [...]}`, even for a single doc. Use this for tag cleanups, applying a new label to N docs in one call, or migrating docs between purposes.

## Params

| Name | Type | Default | Description |
|---|---|---|---|
| `items` | [RetagItem] | (required) | One per doc. |

### RetagItem

| Name | Type | Default | Description |
|---|---|---|---|
| `id` | string | (required) | Doc id. |
| `doc_type` | string | (auto) | `thoughts`/`entities`/`questions`/`conclusions`/`reasons`. Auto-detected when omitted. |
| `add_tags` | [string] | — | Tags to append (idempotent — duplicates skipped). |
| `remove_tags` | [string] | — | Tags to strip (case-sensitive exact match). |
| `purpose` | string | — | New purpose tag. Replaces the current purpose tag in `tags` and the frontmatter `purpose` field. Must be a known purpose (`purpose({action:"list"})`). |

Apply order per item: `remove_tags` → `purpose` swap → `add_tags`.

## Returns

```json
{
  "count": 2,
  "ok": 2,
  "errors": 0,
  "results": [
    {
      "index": 0,
      "ok": true,
      "value": {
        "id": "abc-123",
        "doc_type": "thoughts",
        "tags_before": ["thought", "alpha", "draft"],
        "tags_after":  ["thought", "beta", "reviewed"],
        "purpose": "beta"
      }
    }
  ]
}
```

## Examples

Bulk-add a label to many docs:
```json
{ "items": [
  { "id": "abc-1", "add_tags": ["reviewed"] },
  { "id": "abc-2", "add_tags": ["reviewed"] }
] }
```

Move many docs from purpose `alpha` to `beta`:
```json
{ "items": [
  { "id": "abc-1", "purpose": "beta" },
  { "id": "abc-2", "purpose": "beta" }
] }
```

Combine: strip a stale tag, add a new one, and move purpose in one call:
```json
{ "items": [
  { "id": "abc-1", "remove_tags": ["draft"], "add_tags": ["reviewed"], "purpose": "beta" }
] }
```

## Notes

- Items processed sequentially; per-item failures do not abort the batch.
- `purpose` swap strips any existing tag matching a known purpose tag plus the doc's current `purpose` frontmatter — only the new purpose remains.
- Unknown `purpose` returns an item-level error; create the purpose first via `purpose({action:"create", ...})`.
- For full-body edits, use `update`. For deletion, use `delete_doc`.
- Re-embed is **not** triggered (content unchanged). Tag-driven indexes (`tag_index`, `purpose_index`) refresh on the next read since they're rebuilt from frontmatter.
