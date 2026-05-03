# learn_from_feedback

Replay accumulated `.wiki/feedback.jsonl` entries into the graph. For each entry, classifies picked vs. dropped candidates and emits typed edges (`Supports`, `Contradicts`, `References`, etc.).

## Params

| Name | Type | Default | Description |
|---|---|---|---|
| `limit` | int | 25 | Max number of feedback entries to process per run. |
| `dry_run` | bool | `false` | If `true`, classify and report without writing edges. |

## Returns

JSON report with edges emitted, entries processed, and any errors.

## Notes

- Feedback entries are written to `.wiki/feedback.jsonl` whenever a search result is accepted or rejected by the user (or agent).
- Processed entries are not removed from the log — re-running is safe (duplicate edges are deduplicated by the store).
- Run periodically after interactive search sessions to incorporate implicit signal into the graph.
- `dry_run=true` shows what edges would be emitted without modifying the vault.
