param([string]$Uri)

# Strip union:/// prefix
$raw = $Uri -replace '^union:///', ''

# Extract OS tag (default to 'win' if absent)
$sourceOs = 'win'
if ($raw -match '^(win|mac|lin)/') {
    $sourceOs = $Matches[1]
    $raw = $raw -replace '^(win|mac|lin)/', ''
}

# URL-decode
$path = [System.Uri]::UnescapeDataString($raw)

# Translate path if source OS differs from Windows
if ($sourceOs -ne 'win') {
    $settingsPath = Join-Path $env:LOCALAPPDATA 'ufb\settings.json'
    if (Test-Path $settingsPath) {
        try {
            $settings = Get-Content $settingsPath -Raw | ConvertFrom-Json
            foreach ($mapping in $settings.pathMappings) {
                $sourcePrefix = $mapping.$sourceOs
                if ($sourcePrefix -and $path.StartsWith($sourcePrefix)) {
                    $winPrefix = $mapping.win
                    if ($winPrefix) {
                        $path = $winPrefix + $path.Substring($sourcePrefix.Length)
                        break
                    }
                }
            }
        } catch {
            # Settings unreadable — continue with untranslated path
        }
    }
}

# Convert forward slashes to backslashes for Windows
$path = $path -replace '/', '\'

if (Test-Path $path) {
    explorer.exe /select,"$path"
} else {
    $parent = Split-Path $path -Parent
    if ($parent -and (Test-Path $parent)) {
        explorer.exe "$parent"
    } else {
        explorer.exe "$path"
    }
}
