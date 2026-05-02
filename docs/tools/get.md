# get

Fetch a single doc with reasons and 1-hop neighbors always included. Use when you have an `id` from `search` and need deeper context or a wider edge walk.

## Params

| Name | Type | Default | Description |
|---|---|---|---|
| `id` | string | (required) | Doc id (e.g. `conclusions/learning/why-x`). |
| `depth` | int | 1 | Edge-walk depth. `1` = direct neighbors, `2` = neighbors-of-neighbors, etc. |

## Returns

```json
{
  "doc": { "id": "...", "type": "conclusion", "title": "...", "body": "...", "tags": [...] },
  "reasons": [{ "kind": "Answers", "from": "...", "to": "..." }],
  "edges_by_depth": {
    "1": [{ "id": "...", "kind": "References", "type": "thought" }],
    "2": [...]
  }
}
```

## Examples

```json
{ "id": "conclusions/learning/why-x" }
```
```json
{ "id": "questions/learning/q-abc", "depth": 2 }
```

## Notes

- Behavior change from old `get`: reasons + 1-hop edges are always included; no separate `search_reasons_for` call needed.
- `depth=0` returns only the doc + its direct reasons (no edge walk).
- Use `delete_doc` for removal; use `update` to change the body.
