$ErrorActionPreference = 'Stop'

# 1. Check for Admin Privileges
$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) {
    Write-Error "Error: This installer must be run with Administrator privileges."
    Exit 1
}

Write-Output "Checking for the latest DiskTracker version from GitHub..."
$repoUrl = "https://api.github.com/repos/pratham15541/disktracker/releases/latest"

try {
    # Resolve the latest release info
    $release = Invoke-RestMethod -Uri $repoUrl -Headers @{ "User-Agent" = "DiskTracker-Installer" } -ErrorAction Stop
    $tag = $release.tag_name
} catch {
    Write-Error "Failed to fetch release info from GitHub. Please check your network connection."
    Exit 1
}

Write-Output "Latest version found: $tag"
$zipUrl = "https://github.com/pratham15541/disktracker/releases/download/$tag/disktracker-$tag-windows-x64.zip"
$tempZip = Join-Path $env:TEMP "disktracker-$tag.zip"

Write-Output "Downloading from $zipUrl..."
try {
    Invoke-WebRequest -Uri $zipUrl -OutFile $tempZip -UseBasicParsing -ErrorAction Stop
} catch {
    Write-Error "Failed to download DiskTracker ZIP file from GitHub."
    Exit 1
}

$installDir = "C:\Program Files\DiskTracker"
if (-not (Test-Path $installDir)) {
    New-Item -ItemType Directory -Path $installDir | Out-Null
}

Write-Output "Extracting DiskTracker to $installDir..."
try {
    # Extract ZIP
    Expand-Archive -Path $tempZip -DestinationPath $installDir -Force
} catch {
    Write-Error "Failed to extract DiskTracker ZIP."
    Remove-Item -Path $tempZip -ErrorAction SilentlyContinue
    Exit 1
}

# Clean up ZIP
Remove-Item -Path $tempZip -ErrorAction SilentlyContinue

# Update Path System Environment Variable
Write-Output "Updating system PATH..."
$pathEnv = [Environment]::GetEnvironmentVariable("Path", [EnvironmentVariableTarget]::Machine)
$paths = $pathEnv -split ';'
if ($paths -notcontains $installDir) {
    [Environment]::SetEnvironmentVariable("Path", $pathEnv + ";$installDir", [EnvironmentVariableTarget]::Machine)
    Write-Output "Added $installDir to System PATH."
} else {
    Write-Output "$installDir is already in System PATH."
}

# Inform about session reload
Write-Output ""
Write-Output "=================================================="
Write-Output "DiskTracker $tag installed successfully!"
Write-Output "Please restart your terminal to use the 'disktracker' command."
Write-Output "=================================================="
