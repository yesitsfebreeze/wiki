# code_index

Index source code under a directory via tree-sitter. Writes structure outlines (symbol maps) and fn bodies to `.wiki/code/`.

## Params

| Name | Type | Default | Description |
|---|---|---|---|
| `src_dir` | string | (required) | Absolute or repo-relative path to the directory to index. |
| `ext` | string | `rs` | File extension to index (`rs`, `ts`, `py`, etc.). |

## Returns

Summary string with file and symbol counts.

## Notes

- Called automatically by the code-read-hook on file change; manual call re-indexes a directory without waiting for a hook.
- Overwrites existing index entries for changed files — idempotent.
- After indexing, use `code_search` to grep bodies/skeletons, `code_read` to fetch individual outlines or fn bodies, and `code_validate` to check index health.
- Large directories (>10k files) may take several seconds; scope `src_dir` narrowly if possible.
