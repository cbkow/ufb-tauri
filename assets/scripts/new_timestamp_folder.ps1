param(
    [string]$TargetDir,
    [string]$Format = "yyMMdd"
)

$folderName = Get-Date -Format $Format
New-Item -ItemType Directory -Path (Join-Path $TargetDir $folderName) -Force | Out-Null
