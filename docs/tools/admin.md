# admin

Vault maintenance. Single tool for recompute, sanitize, migrate, feedback.

## Params

| Name | Type | Default | Description |
|---|---|---|---|
| `action` | enum | (required) | `recompute` \| `sanitize` \| `migrate` \| `feedback` |
| `dry_run` | bool | `false` | `recompute` / `migrate` / `feedback`: count without writing. |
| `limit` | int | `25` | `feedback`: max entries to replay. |

## Actions

### recompute
Recompute pagerank-style node weights across the vault. Writes `node_size` to each doc's frontmatter. Run after large ingest batches when importance scores feel stale.

### sanitize
Rename vault docs whose stems contain characters Obsidian cannot wikilink (spaces, parens, slashes, etc.), then rewrite all `[[wikilinks]]` and relative `.md` links vault-wide. Idempotent.

### migrate
Delete legacy template-shaped questions (e.g. "How does X relate to similar concepts?") created before the template-filter was tightened. Skips any question with inbound `Answers` edges. Idempotent — safe to run repeatedly.

### feedback
Replay `.wiki/feedback.jsonl` into the graph: classify picked vs dropped candidates and emit typed edges (Supports/Contradicts/etc.). Use `limit` to process incrementally.

## Notes

- Replaces `recompute_weights`, `sanitize`, `migrate_templated_questions`, `learn_from_feedback`.
- `sanitize` does not take `dry_run` — run it; it's idempotent.
