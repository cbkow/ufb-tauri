param(
    [string]$InstDir,
    [string]$ExeName
)

$nilesoftDir = Join-Path $env:ProgramFiles 'Nilesoft Shell'
$importsDir  = Join-Path $nilesoftDir 'imports'
$srcDir      = Join-Path $InstDir 'assets\shell\import'

# Bail if Nilesoft Shell not installed
if (-not (Test-Path (Join-Path $nilesoftDir 'shell.nss'))) { exit 0 }

# Backup originals (only if no backup already exists)
$shellNss = Join-Path $nilesoftDir 'shell.nss'
$shellBak = Join-Path $nilesoftDir 'shell.nss.bak'
if ((Test-Path $shellNss) -and -not (Test-Path $shellBak)) {
    Copy-Item $shellNss $shellBak -Force
}

$modifyNss = Join-Path $importsDir 'modify.nss'
$modifyBak = Join-Path $importsDir 'modify.nss.bak'
if ((Test-Path $modifyNss) -and -not (Test-Path $modifyBak)) {
    Copy-Item $modifyNss $modifyBak -Force
}

# Copy our shell.nss
Copy-Item (Join-Path $InstDir 'assets\shell\shell.nss') $shellNss -Force

# Create imports dir if missing
if (-not (Test-Path $importsDir)) { New-Item -ItemType Directory -Path $importsDir -Force | Out-Null }

# Copy and patch each .nss import file
Get-ChildItem $srcDir -Filter '*.nss' | ForEach-Object {
    $content = Get-Content $_.FullName -Raw
    $content = $content -replace [regex]::Escape('{{INSTDIR}}'), $InstDir
    $content = $content -replace [regex]::Escape('{{EXENAME}}'), $ExeName
    Set-Content -Path (Join-Path $importsDir $_.Name) -Value $content -NoNewline
}
