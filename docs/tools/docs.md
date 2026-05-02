# docs

Lists or reads bundled wiki documentation. Entry point for agents to discover the tool surface and concept docs without leaving the MCP session.

## Params

| Name | Type | Default | Description |
|---|---|---|---|
| `name` | string | (none) | Doc path. Omit to list all docs. Resolves against `tools/`, `concepts/`, and root. |

## Returns

```json
{ "docs": ["tools/search", "tools/get", "concepts/overview", ...] }
```
or
```json
{ "name": "tools/search", "content": "<markdown>" }
```

## Examples

```json
{ "name": "tools/ingest" }
```
```json
{}
```

## Notes

- Index is compiled into the binary at build time (`include_dir!`).
- Path resolution tries `<name>.md`, then `tools/<name>.md`, then `concepts/<name>.md`.
- This is the canonical place to look up the new consolidated tool surface.
