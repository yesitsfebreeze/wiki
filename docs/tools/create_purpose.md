# create_purpose

Create a new purpose bucket. Purposes partition the vault into topic domains; docs are classified into them via embedding similarity at ingest time.

## Params

| Name | Type | Default | Description |
|---|---|---|---|
| `tag` | string | (required) | Short slug, e.g. `learning`. Used as the purpose tag on docs. |
| `title` | string | (required) | Human-readable label shown in UI and reports. |
| `description` | string | (required) | Full description used for embedding classification. |

## Returns

The created purpose object: `{ tag, title, description, path }`.

## Notes

- The `description` drives embedding classification — write it as a rich paragraph describing what belongs in this purpose. Thin descriptions produce poor auto-classification.
- After creating a purpose, call `reembed_purposes` to rebuild `.vec` sidecar files if you want the new purpose to be immediately active for classification.
- Tag must be unique; duplicate tags return an error.
