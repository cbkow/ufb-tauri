param(
    [string]$Path,
    [string]$Mode = "doc"  # "doc" or "folder"
)

$appRoot = Split-Path (Split-Path $PSScriptRoot)

# Job root prefixes (case-insensitive)
$jobRoots = @(
    "D:\Jobs_Live",
    "C:\Volumes\union-ny-gfx\union-jobs"
)

# Try to extract job name (NNNNNN_name) from the path
$jobName = $null
foreach ($root in $jobRoots) {
    if ($Path -like "$root\*") {
        # Get the portion after the root
        $relative = $Path.Substring($root.Length + 1)
        # First path segment is the job folder
        $jobFolder = $relative.Split('\')[0]
        # Validate it matches NNNNNN_name pattern
        if ($jobFolder -match '^\d{6}_') {
            $jobName = $jobFolder
        }
        break
    }
}

if (-not $jobName) {
    [System.Reflection.Assembly]::LoadWithPartialName("System.Windows.Forms") | Out-Null
    [System.Windows.Forms.MessageBox]::Show(
        "Could not detect a project from this path.`n`nExpected a path under:`n  D:\Jobs_Live\NNNNNN_jobname`n  C:\Volumes\union-ny-gfx\union-jobs\NNNNNN_jobname",
        "Project Notes",
        [System.Windows.Forms.MessageBoxButtons]::OK,
        [System.Windows.Forms.MessageBoxIcon]::Warning
    ) | Out-Null
    exit
}

# Read config
$configPath = Join-Path $appRoot 'assets\google_config\config.json'
if (-not (Test-Path $configPath)) {
    [System.Reflection.Assembly]::LoadWithPartialName("System.Windows.Forms") | Out-Null
    [System.Windows.Forms.MessageBox]::Show(
        "Config file not found at:`n$configPath",
        "Project Notes",
        [System.Windows.Forms.MessageBoxButtons]::OK,
        [System.Windows.Forms.MessageBoxIcon]::Warning
    ) | Out-Null
    exit
}

$config = Get-Content $configPath -Raw | ConvertFrom-Json
$scriptUrl = $config.google_drive.notes_script_url
$parentFolderId = $config.google_drive.parent_folder_id

if (-not $scriptUrl -or -not $parentFolderId) {
    [System.Reflection.Assembly]::LoadWithPartialName("System.Windows.Forms") | Out-Null
    [System.Windows.Forms.MessageBox]::Show(
        "Google Drive not configured in config.json.`n`nEnsure notes_script_url and parent_folder_id are set.",
        "Project Notes",
        [System.Windows.Forms.MessageBoxButtons]::OK,
        [System.Windows.Forms.MessageBoxIcon]::Warning
    ) | Out-Null
    exit
}

# URL-encode the job name and parent folder ID
$encodedJob = [System.Uri]::EscapeDataString($jobName)
$encodedParent = [System.Uri]::EscapeDataString($parentFolderId)

$url = "$scriptUrl`?job=$encodedJob&parent=$encodedParent&mode=$Mode"

Start-Process $url
