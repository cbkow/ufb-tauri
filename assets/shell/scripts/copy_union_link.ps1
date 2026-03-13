param([string]$Path)

# Convert to URL-friendly format with OS prefix
$urlPath = $Path -replace '\\', '/'
$encoded = [System.Uri]::EscapeDataString($urlPath)
$unionLink = "union:///win/$encoded"

Set-Clipboard -Value $unionLink
