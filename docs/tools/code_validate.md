# code_validate

Validate the code index for consistency issues: orphan body files, dangling structure references, and missing fn bodies.

## Params

| Name | Type | Default | Description |
|---|---|---|---|
| `fix` | bool | `false` | If `true`, automatically repair safe issues (remove orphans, clean dangling refs). Unsafe issues (missing bodies) are reported only. |

## Returns

```json
{
  "orphans": 2,
  "dangling_refs": 1,
  "missing_bodies": 0,
  "problems": ["orphan: .wiki/code/rs/functions/src/foo/bar", "..."],
  "fixed": 3
}
```

## Notes

- Run `fix=false` first (dry-run) to see what would change before committing repairs.
- Orphans = body file exists but no structure entry references it (safe to remove).
- Dangling refs = structure entry points to a body file that doesn't exist (safe to clean from structure).
- Missing bodies = structure entry exists but body file is absent — not auto-fixed; re-run `code_index` on the affected directory.
