param([string]$Path)

# Convert to URL-friendly format with OS prefix
$urlPath = $Path -replace '\\', '/'
$encoded = [System.Uri]::EscapeDataString($urlPath)
$ufbLink = "ufb:///win/$encoded"

Set-Clipboard -Value $ufbLink
