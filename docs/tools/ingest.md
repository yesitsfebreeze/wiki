# ingest

Batch-only. Single write tool for all five doc types. Wrap every payload in `{items: [...]}`, even for a single doc. Server-side invariants embed the body, auto-link to nearest neighbors, resolve `[[wikilinks]]`, and (for conclusions and body-start question wikilinks) auto-mark matching open questions as answered.

## Params

| Name | Type | Default | Description |
|---|---|---|---|
| `items` | [IngestItem] | (required) | One per doc to write. |

### IngestItem

| Name | Type | Default | Description |
|---|---|---|---|
| `kind` | enum | (required) | `"thought"`, `"entity"`, `"question"`, `"reason"`, `"conclusion"`. |
| `body` | string | (required) | Doc content. Wikilinks `[[target]]` are parsed and linked. |
| `title` | string | (optional) | Override the auto-derived title. |
| `tags` | [string] | `[]` | Extra tags. Purpose tag is auto-classified if not provided. |
| `refs` | [string] | `[]` | Explicit `References` edges from this doc to each id. |
| `purpose_hint` | string | (auto) | Force a purpose tag instead of classifying from body. |
| `from_id` / `to_id` / `reason_kind` | — | — | Required only when `kind="reason"`. |

## Returns

```json
{
  "count": 2,
  "ok": 2,
  "errors": 0,
  "results": [
    {
      "index": 0,
      "ok": true,
      "value": {
        "ingested": { "id": "...", "title": "...", ... },
        "auto_linked": [ { "id": "...", "kind": "Supports" } ],
        "promoted": { "question_id": "...", "marked": "answered" }
      }
    },
    {
      "index": 1,
      "ok": true,
      "value": {
        "ingested": {
          "id": "...", "title": "...", "tags": [...], "purpose": "...", "content": "...",
          "merged": { "merged_into": "...", "existing_title": "...", "alias_added": null,
                      "note": "near-duplicate found — merged as alias, no new doc created" }
        },
        "auto_linked": []
      }
    }
  ]
}
```

`promoted` is present only when `kind=conclusion` matches an open question, or when `kind=question` matches an existing conclusion. Entity near-duplicate merges return the **same top-level shape as a fresh doc** (`id`, `title`, `tags`, `purpose`, `content`) plus a nested `merged` object — callers no longer branch on shape.

## Examples

```json
{ "items": [
  { "kind": "thought", "body": "Cosine threshold 0.82 chosen to balance precision and recall." },
  { "kind": "thought", "body": "[[<question_id>]] Threshold 0.82 was chosen empirically — see logs." },
  { "kind": "conclusion", "body": "Auto-link threshold should default to 0.82.", "tags": ["learning"] },
  { "kind": "reason", "body": "Supports", "from_id": "thoughts/a", "to_id": "thoughts/b", "reason_kind": "Supports" }
] }
```

## Notes

- Items processed **sequentially**, not concurrently — wikilink resolution and dedupe-merge depend on prior writes being visible.
- Per-item failures do not abort the batch; check each result's `ok` flag.
- Server-side invariants (per item):
  1. Embed body.
  2. Nearest-neighbor scan within purpose cluster → top-k similar (cosine ≥ 0.82) become edges.
  3. Parse `[[wikilinks]]` → resolve + edge. Body-start wikilink → `Supports` (question target or same-purpose non-question target). **No auto-mark** — `learn_pass` decides promotion from accumulated `Supports`. Mid-body / cross-purpose → `References`. To force an immediate `Answers` edge + auto-mark, ingest an explicit reason: `{kind:"reason", reason_kind:"Answers", from_id, to_id}`.
  4. `kind=conclusion`: scan open questions; match → emit `Answers` edge from new conclusion to the question, repoint inbound wikilinks, then hard-delete the question (lifecycle is open|graveyard|deleted).
  5. `kind=question`: scan existing conclusions; match → emit `Answers` edge from existing conclusion to the new question, repoint inbound, hard-delete the duplicate question.
- `auto_linked` returned so caller can audit. To remove a bad auto-edge, call `delete_doc` on the reason node or `update` to replace.
- Top-5 cap on auto-linked count per item.
- For huge batches, prefer chunks of ≤100 to keep the response payload manageable.
