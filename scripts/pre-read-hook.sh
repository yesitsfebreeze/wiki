#!/usr/bin/env bash
# PreToolUse(Read) hook — only intercept files with wiki-indexed extensions.
# exit 0 = allow normal Read; non-zero / JSON block = use wiki code index instead.
set -euo pipefail

INPUT=$(cat)

# Extract file_path from PreToolUse JSON input
if command -v jq &>/dev/null; then
    FILE_PATH=$(printf '%s' "$INPUT" | jq -r '.tool_input.file_path // empty' 2>/dev/null || echo "")
else
    FILE_PATH=$(printf '%s' "$INPUT" | python3 -c \
        "import json,sys; d=json.load(sys.stdin); print(d.get('tool_input',{}).get('file_path',''))" \
        2>/dev/null || echo "")
fi

[ -z "$FILE_PATH" ] && exit 0

# Extract lowercase extension
EXT="${FILE_PATH##*.}"
EXT=$(printf '%s' "$EXT" | tr '[:upper:]' '[:lower:]')
# No extension (no dot, or dot at start)
[ "$EXT" = "$FILE_PATH" ] || [ -z "$EXT" ] && exit 0

# Built-in supported extensions (compiled-in WASM grammars)
SUPPORTED="rs py"

# Also check project-level language dir for user-installed WASM grammars
LANG_DIR="${CLAUDE_PROJECT_DIR:-$PWD}/.wiki/code/languages"
if [ -d "$LANG_DIR" ]; then
    for f in "$LANG_DIR"/*.wasm; do
        [ -f "$f" ] && SUPPORTED="$SUPPORTED $(basename "$f" .wasm)"
    done
fi

# Check user-level language dir
if [ -n "${HOME:-}" ] && [ -d "$HOME/.config/split/languages" ]; then
    for f in "$HOME/.config/split/languages"/*.wasm; do
        [ -f "$f" ] && SUPPORTED="$SUPPORTED $(basename "$f" .wasm)"
    done
fi

# If extension not in supported set, allow normal Read
MATCH=0
for supported_ext in $SUPPORTED; do
    [ "$EXT" = "$supported_ext" ] && MATCH=1 && break
done
[ $MATCH -eq 0 ] && exit 0

# Extension is wiki-indexed — delegate to wiki code-read-hook
WIKI="${CLAUDE_PLUGIN_ROOT:-.}/bin/wiki"
printf '%s' "$INPUT" | "$WIKI" code-read-hook
exit $?
