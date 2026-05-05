# mark_question

Batch-only. Manually transition status on many questions in one call. Wrap every payload in `{items: [...]}`, even for a single update. Mostly auto-driven by `learn_pass` and body-start `[[<question_id>]]` wikilinks; use this only when overriding.

## Params

| Name | Type | Default | Description |
|---|---|---|---|
| `items` | [MarkQuestionItem] | (required) | One per question. |

### MarkQuestionItem

| Name | Type | Default | Description |
|---|---|---|---|
| `question_id` | string | (required) | Question doc id. |
| `status` | enum | (required) | `"answered"`, `"dropped"`. |

## Returns

```json
{
  "count": 2,
  "ok": 2,
  "errors": 0,
  "results": [
    { "index": 0, "ok": true, "value": { "question_id": "abc", "status": "answered" } },
    { "index": 1, "ok": true, "value": { "question_id": "def", "status": "dropped" } }
  ]
}
```

## Examples

```json
{ "items": [
  { "question_id": "abc-123", "status": "answered" },
  { "question_id": "def-456", "status": "dropped" }
] }
```

## Notes

- The doc-side tag is overwritten: any prior `answered`/`dropped` tag is removed before the new one is appended.
- `answered` moves the file under `questions/answered/`; `dropped` under `questions/dropped/`.
- `learn_pass` already auto-marks; only call directly to override.
- Items processed sequentially; per-item failures do not abort the batch.
