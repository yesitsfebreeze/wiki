# sanitize

Sanitize vault filenames and rewrite all internal links vault-wide. Renames docs whose stem contains characters Obsidian cannot wikilink (spaces, parentheses, slashes, etc.), then updates every `[[wikilink]]` and relative `.md` link across the vault to point at the new names.

## Params

None.

## Returns

```json
{ "renamed": 7, "links_rewritten": 43 }
```

## Notes

- Idempotent: running again on an already-clean vault returns `renamed: 0, links_rewritten: 0`.
- Safe to run at any time — only affects filenames that contain problematic characters.
- Rewriting touches many files; if the vault is under git, review the diff after running.
- Does not affect doc content, tags, or graph edges — only filesystem names and link text.
