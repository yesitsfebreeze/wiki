# mark_question

Batch-only. Resolve open questions in bulk. Wrap every payload in `{items: [...]}`, even for a single update.

`learn_pass` auto-promotes questions to conclusions (which deletes them); use `mark_question` only to manually delete junk/duplicates or to bury an unanswerable question into the graveyard.

## Lifecycle model

Two states only:

- **open** — `questions/<purpose>/...`. Default state for any new question.
- **buried** — `questions/graveyard/<purpose>/...`. Junk or unanswerable. Excluded from `list_open_questions`, `learn_pass` raise/QA passes, and conclusions-first search.

Anything else is hard-deleted. An "answered" question is one that has been promoted: a conclusion doc now exists with an `Answers`/`Derives` edge to the (former) question, and the question file itself is gone.

To re-explore everything in the graveyard, delete the `questions/graveyard/` directory.

## Params

| Name | Type | Default | Description |
|---|---|---|---|
| `items` | [MarkQuestionItem] | (required) | One per question. |

### MarkQuestionItem

| Name | Type | Default | Description |
|---|---|---|---|
| `question_id` | string | (required) | Question doc id. |
| `status` | enum | (required) | `"deleted"` or `"buried"`. |

- `deleted` — hard-delete the question file. Cascade-deletes every reason (edge) touching it. Use for junk, duplicates, or questions that have already been answered without going through `promote_to_conclusion`.
- `buried` — move the question to `questions/graveyard/<purpose>/`, tag it `"graveyard"`, and rewrite inbound wikilinks to the new path. Reversible: move the file back into the open tree to resurrect.

## Returns

```json
{
  "count": 2,
  "ok": 2,
  "errors": 0,
  "results": [
    { "index": 0, "ok": true, "value": { "question_id": "abc", "status": "deleted", "applied": true } },
    { "index": 1, "ok": true, "value": { "question_id": "def", "status": "buried", "applied": true } }
  ]
}
```

`applied: false` means the question was already in the requested terminal state (idempotent re-bury, or delete on a missing file).

## Examples

```json
{ "items": [
  { "question_id": "abc-123", "status": "deleted" },
  { "question_id": "def-456", "status": "buried" }
] }
```

## Notes

- `deleted` is irreversible — there is no audit trail. If you might need the question text later, use `buried` instead.
- `buried` keeps the file on disk but the cached `graveyard` tag and the path-prefix together exclude it from every learn-pass code path.
- `learn_pass` already auto-promotes (which hard-deletes the question once a conclusion exists); call `mark_question` only to override.
- Items processed sequentially; per-item failures do not abort the batch.
