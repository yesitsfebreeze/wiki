# delete_purpose

Delete a purpose definition by tag.

## Params

| Name | Type | Default | Description |
|---|---|---|---|
| `tag` | string | (required) | The purpose tag to delete. |

## Returns

Confirmation string on success. Error if tag not found.

## Notes

- Deleting a purpose does NOT delete docs tagged with it. Existing docs retain the tag string — they simply no longer map to a live purpose bucket.
- After deletion, run `reembed_purposes` to clean up stale `.vec` sidecars.
- To inspect which docs use a tag before deleting, run `list doc_type=thoughts` (or other types) and filter by purpose, or use `search mode=tag query=<tag>`.
