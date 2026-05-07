# author

Vault maintenance. Single tool for reindex, sanitize, migrate, feedback.

## Params

| Name | Type | Default | Description |
|---|---|---|---|
| `action` | enum | (required) | `reindex` \| `sanitize` \| `migrate` \| `feedback` |
| `dry_run` | bool | `false` | `reindex` / `migrate` / `feedback`: count without writing. |
| `limit` | int | `25` | `feedback`: max entries to replay. |

## Actions

### reindex
Sync `## Relations` wikilinks into all doc bodies from existing reason edges (thoughts, questions, conclusions, entities), then recompute pagerank-style node weights. Run after large ingest or learn batches. Order is intentional: links first, weights after (weights depend on edge counts).

### sanitize
Rename vault docs whose stems contain characters Obsidian cannot wikilink (spaces, parens, slashes, etc.), then rewrite all `[[wikilinks]]` and relative `.md` links vault-wide. Idempotent.

### migrate
Delete legacy template-shaped questions (e.g. "How does X relate to similar concepts?") created before the template-filter was tightened. Skips any question with inbound `Answers` edges. Idempotent — safe to run repeatedly.

### feedback
Replay `.wiki/feedback.jsonl` into the graph: classify picked vs dropped candidates and emit typed edges (Supports/Contradicts/etc.). Use `limit` to process incrementally.

## Notes

- Replaces `recompute_weights`, `sanitize`, `migrate_templated_questions`, `learn_from_feedback`.
- `sanitize` does not take `dry_run` — run it; it's idempotent.
