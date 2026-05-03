# migrate_templated_questions

One-shot cleanup: delete legacy template-shaped questions created before the template-filter was tightened (e.g. "How does X relate to similar concepts?", "What are the implications of Y?").

## Params

| Name | Type | Default | Description |
|---|---|---|---|
| `dry_run` | bool | `false` | If `true`, report what would be deleted without making changes. |

## Returns

```json
{ "deleted": 12, "skipped_has_answers": 3, "dry_run": false }
```

## Notes

- Skips any question that has inbound `Answers` edges — those have been answered and should be preserved.
- Idempotent: running again after cleanup returns `deleted: 0`.
- Use `dry_run=true` first to verify the candidate set before committing deletes.
- Only needed once after upgrading from a pre-filter vault; safe to skip on fresh installs.
