$nilesoftDir = Join-Path $env:ProgramFiles 'Nilesoft Shell'
$importsDir  = Join-Path $nilesoftDir 'imports'

# Bail if Nilesoft Shell not installed
if (-not (Test-Path $nilesoftDir)) { exit 0 }

# Remove our import files
$ourFiles = @('union_files.nss','union_folders.nss','union_projects.nss','union_goto.nss','union_terminal.nss','taskbar.nss','modify.nss')
foreach ($f in $ourFiles) {
    $path = Join-Path $importsDir $f
    if (Test-Path $path) { Remove-Item $path -Force }
}

# Restore backups
$shellBak = Join-Path $nilesoftDir 'shell.nss.bak'
if (Test-Path $shellBak) {
    Copy-Item $shellBak (Join-Path $nilesoftDir 'shell.nss') -Force
    Remove-Item $shellBak -Force
}

$modifyBak = Join-Path $importsDir 'modify.nss.bak'
if (Test-Path $modifyBak) {
    Copy-Item $modifyBak (Join-Path $importsDir 'modify.nss') -Force
    Remove-Item $modifyBak -Force
}
