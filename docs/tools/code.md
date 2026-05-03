# code

Code index operations. Single tool for index, search, read, refs, validate.

## Params

| Name | Type | Default | Description |
|---|---|---|---|
| `action` | enum | (required) | `index` \| `search` \| `read` \| `refs` \| `validate` |
| `src_dir` | string | — | `index`: directory to index. |
| `ext` | string | `"rs"` | `index`: file extension. |
| `query` | string | — | `search`: grep/semantic query. |
| `regex` | bool | `false` | `search`: treat query as regex. |
| `scope` | string | `"body"` | `search`: `body` \| `structure`. |
| `path` | string | — | `read` / `refs`: file path or index path. |
| `symbol` | string | — | `read` / `refs`: bare fn name or `module::fn`. |
| `granularity` | string | `"outline"` | `read`: `outline` \| `file` \| `fn`. |
| `direction` | string | `"both"` | `refs`: `in` \| `out` \| `both`. |
| `depth` | int | `1` | `refs`: walk depth for fn_tree. |
| `fix` | bool | `false` | `validate`: repair safe issues. |
| `cursor` | int | `0` | `search`: pagination offset. |
| `limit` | int | `100` | `search`: max results. |

## Actions

### index
Parse source files via tree-sitter. Writes structure outlines + fn bodies to `.wiki/code/<ext>/`. Called automatically by `code-read-hook` on file change.

### search
Grep indexed fn bodies and skeletons. Supports literal, regex, and semantic queries.

### read
Read indexed code at three granularities: `outline` (symbol map of a file), `file` (full source via index), `fn` (one fn body by path or symbol name).

### refs
Walk the code reference graph. Resolves bare fn names to index paths. `depth > 1` also runs `fn_tree` for deeper call graph.

### validate
Detect orphans (body with no structure entry), dangling refs, missing bodies. With `fix=true`, repairs safe issues.

## Notes

- Replaces `code_index`, `code_search`, `code_read`, `code_refs`, `code_validate`.
- `read` accepts either `path` (exact index path) or `symbol` (bare fn name, auto-resolved).
- `refs` symbol resolution priority: exact path → exact stem match → suffix stem match → unresolved.
