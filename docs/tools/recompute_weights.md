# recompute_weights

Recompute pagerank-style node importance weights across the vault. Writes `node_size` to each doc's frontmatter.

## Params

| Name | Type | Default | Description |
|---|---|---|---|
| `dry_run` | bool | `false` | If `true`, compute weights and return counts without writing to frontmatter. |

## Returns

```json
{ "recomputed": 342, "dry_run": false }
```

## Notes

- `node_size` is used by graph visualizations (e.g. Obsidian graph view) and by `learn_pass` weighted sampling — stale weights cause the pass to over-sample low-value nodes.
- Run after large ingest batches (100+ new docs) or after `migrate_templated_questions` removes bulk nodes.
- The algorithm is proportional to vault size; on large vaults (10k+ docs) this may take several seconds.
- `dry_run=true` is useful to verify the count before a destructive batch.
