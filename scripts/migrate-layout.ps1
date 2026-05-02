#requires -Version 7
<#
.SYNOPSIS
    One-shot migration: flat `<type>/<purpose>-<slug>.md` -> hierarchical
    `<type>/<purpose>/<slug>.md`. Rewrites legacy `[[<old-slug>|...]]` wikilinks
    to the new `[[<doc_type>/<purpose>/<new-slug>|...]]` form. Wipes `.search/`
    so tantivy reindexes on next query.

.PARAMETER WikiPath
    Absolute path to the `.wiki` vault. Required.

.EXAMPLE
    pwsh ./scripts/migrate-layout.ps1 -WikiPath C:\Users\you\dev\proj\.wiki

.NOTES
    Idempotent. Re-running on an already-migrated vault is a no-op.
    Commit your vault first if you want a clean revert path.
#>
param(
    [Parameter(Mandatory = $true)]
    [string]$WikiPath
)

$ErrorActionPreference = 'Stop'

if (-not (Test-Path -LiteralPath $WikiPath -PathType Container)) {
    throw "WikiPath not found or not a directory: $WikiPath"
}

$docTypes = @('thoughts', 'entities', 'reasons', 'questions', 'conclusions')

# ── Phase 1: plan moves ────────────────────────────────────────────────
$plan = New-Object System.Collections.Generic.List[pscustomobject]
$slugMap = @{}
$plannedDsts = New-Object System.Collections.Generic.HashSet[string]
$skipped = 0

foreach ($dt in $docTypes) {
    $dir = Join-Path $WikiPath $dt
    if (-not (Test-Path -LiteralPath $dir -PathType Container)) { continue }

    Get-ChildItem -LiteralPath $dir -Filter *.md -File | ForEach-Object {
        $file = $_
        $raw  = Get-Content -LiteralPath $file.FullName -Raw -Encoding utf8

        $purpose = 'uncategorized'
        if ($raw -match '(?s)^---\s*\r?\n(.*?)\r?\n---\s*\r?\n') {
            $fm = $matches[1]
            if ($fm -match '(?m)^\s*purpose:\s*(.+?)\s*$') {
                $purpose = $matches[1].Trim().Trim('"').Trim("'")
                if ([string]::IsNullOrWhiteSpace($purpose)) { $purpose = 'uncategorized' }
            }
        }

        $stem = $file.BaseName
        $prefix = "$purpose-"
        $bare = if ($stem.StartsWith($prefix)) { $stem.Substring($prefix.Length) } else { $stem }

        $newDir  = Join-Path $dir $purpose
        $newPath = Join-Path $newDir "$bare.md"
        $suffix  = 1
        while ((Test-Path -LiteralPath $newPath) -or $plannedDsts.Contains($newPath)) {
            $newPath = Join-Path $newDir "$bare-$suffix.md"
            $suffix++
        }
        [void]$plannedDsts.Add($newPath)

        $newStem  = [System.IO.Path]::GetFileNameWithoutExtension($newPath)
        $slugMap[$stem] = "$dt/$purpose/$newStem"

        $plan.Add([pscustomobject]@{
            Src = $file.FullName
            Dst = $newPath
            DocType = $dt
        })
    }
}

# ── Phase 2: execute moves ─────────────────────────────────────────────
$byType = @{}
foreach ($m in $plan) {
    $parent = Split-Path -Parent $m.Dst
    New-Item -ItemType Directory -Force -Path $parent | Out-Null
    Move-Item -LiteralPath $m.Src -Destination $m.Dst
    if (-not $byType.ContainsKey($m.DocType)) { $byType[$m.DocType] = 0 }
    $byType[$m.DocType]++
}

# ── Phase 3: rewrite wikilinks ─────────────────────────────────────────
# Sort slugs longest-first so e.g. `forward-plus-foo` matches before `forward-plus`.
$slugsLongestFirst = $slugMap.Keys | Sort-Object -Property Length -Descending

$filesTouched = 0
foreach ($dt in $docTypes) {
    $dir = Join-Path $WikiPath $dt
    if (-not (Test-Path -LiteralPath $dir -PathType Container)) { continue }

    Get-ChildItem -LiteralPath $dir -Filter *.md -File -Recurse | ForEach-Object {
        $file = $_
        $raw  = Get-Content -LiteralPath $file.FullName -Raw -Encoding utf8
        $orig = $raw

        foreach ($slug in $slugsLongestFirst) {
            $pat = "[[$slug"
            if (-not $raw.Contains($pat)) { continue }
            $newTarget = $slugMap[$slug]
            $escaped = [regex]::Escape($slug)
            # Match `[[<slug>` followed by `]`, `|`, or `#` — wikilink terminator/modifier.
            $regex = "\[\[$escaped(?<rest>[\]|#])"
            $raw = [regex]::Replace($raw, $regex, "[[$newTarget`${rest}")
        }

        if ($raw -ne $orig) {
            Set-Content -LiteralPath $file.FullName -Value $raw -Encoding utf8 -NoNewline
            $filesTouched++
        }
    }
}

# ── Phase 4: wipe search index so tantivy rebuilds against new paths ───
$searchDir = Join-Path $WikiPath '.search'
if (Test-Path -LiteralPath $searchDir) {
    Remove-Item -LiteralPath $searchDir -Recurse -Force
}
New-Item -ItemType Directory -Force -Path $searchDir | Out-Null

# ── Report ──────────────────────────────────────────────────────────────
[pscustomobject]@{
    moved                         = $plan.Count
    skipped                       = $skipped
    by_type                       = $byType
    slugs_mapped                  = $slugMap.Count
    files_touched_by_link_rewrite = $filesTouched
    note                          = 'search index wiped; will rebuild on next query'
} | ConvertTo-Json -Depth 4
