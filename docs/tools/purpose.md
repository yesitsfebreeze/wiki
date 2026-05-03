# purpose

Manage purpose buckets. Single tool for list, create, delete, reembed.

## Params

| Name | Type | Default | Description |
|---|---|---|---|
| `action` | enum | (required) | `list` \| `create` \| `delete` \| `reembed` |
| `tag` | string | — | Required for `create` and `delete`. |
| `title` | string | — | Required for `create`. |
| `description` | string | — | Required for `create`. Drives embedding classification — write clearly. |

## Actions

### list
Returns `[{tag, title, description, path}]` for all configured purposes.

### create
Creates a new purpose bucket. The `description` is embedded via OpenAI and used to classify incoming docs via cosine similarity. A vague description produces misclassification.

### delete
Removes the purpose definition. Docs already tagged with this purpose are **not** deleted or re-classified.

### reembed
Drops all `.vec` embedding sidecar files for purposes and rebuilds them. Run after editing `description` fields or after upgrading the embedding model.

## Notes

- Replaces `list_purposes`, `create_purpose`, `delete_purpose`, `reembed_purposes`.
- New purposes take effect immediately for subsequent ingests; existing docs are not re-classified unless re-ingested.
