# list_purposes

List all configured purpose buckets. Use to discover valid purpose tags before creating docs or filtering queries.

## Params

None.

## Returns

Array of purpose objects:

```json
[{ "tag": "learning", "title": "Learning", "description": "..." }]
```

## Notes

- Purpose tags are used to classify docs during ingest and to scope `learn_pass` + `list_open_questions`.
- To add a purpose, use `create_purpose`. To remove, use `delete_purpose`.
