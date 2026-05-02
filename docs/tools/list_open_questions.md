# list_open_questions

Surface open questions across the vault. High-frequency gap-finding tool — call before deciding what to research next.

## Params

| Name | Type | Default | Description |
|---|---|---|---|
| `purpose` | string | (any) | Filter to one purpose tag. |
| `k` | int | 50 | Max questions returned. |

## Returns

```json
{
  "questions": [
    { "id": "questions/learning/q-abc", "title": "...", "body": "...", "purpose": "learning", "support_count": 2 }
  ]
}
```

## Examples

```json
{}
```
```json
{ "purpose": "learning", "k": 20 }
```

## Notes

- `support_count` = number of `Supports` reasons accumulated; high values are good candidates for `search({mode:"qa"})` to look for an emerging answer.
- Resolved/answered/dropped questions excluded.
