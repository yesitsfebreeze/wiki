# PreToolUse(Read) hook — only intercept files with wiki-indexed extensions.
# exit 0 = allow normal Read; non-zero / JSON block = use wiki code index instead.

$inputJson = [Console]::In.ReadToEnd()

try {
    $data = $inputJson | ConvertFrom-Json
    $filePath = $data.tool_input.file_path
} catch {
    exit 0
}

if (-not $filePath) { exit 0 }

$ext = [System.IO.Path]::GetExtension($filePath).TrimStart('.').ToLower()
if (-not $ext) { exit 0 }

# Built-in supported extensions (compiled-in WASM grammars)
$supported = [System.Collections.Generic.List[string]]@('rs', 'py')

# Check project-level language dir for user-installed WASM grammars
$projectDir = if ($env:CLAUDE_PROJECT_DIR) { $env:CLAUDE_PROJECT_DIR } else { $PWD.Path }
$langDir = Join-Path $projectDir ".wiki\code\languages"
if (Test-Path $langDir) {
    Get-ChildItem "$langDir\*.wasm" -ErrorAction SilentlyContinue | ForEach-Object {
        $supported.Add($_.BaseName)
    }
}

# Check user-level language dir
$userLangDir = Join-Path $HOME ".config\split\languages"
if (Test-Path $userLangDir) {
    Get-ChildItem "$userLangDir\*.wasm" -ErrorAction SilentlyContinue | ForEach-Object {
        $supported.Add($_.BaseName)
    }
}

# If extension not in supported set, allow normal Read
if ($ext -notin $supported) { exit 0 }

# Extension is wiki-indexed — delegate to wiki code-read-hook
$wikiExe = Join-Path $env:CLAUDE_PLUGIN_ROOT "bin\wiki.exe"
$inputJson | & $wikiExe code-read-hook
exit $LASTEXITCODE
