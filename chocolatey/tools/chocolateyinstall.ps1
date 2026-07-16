$ErrorActionPreference = 'Stop'

$packageName = 'disktracker'
$url64 = "https://github.com/pratham15541/disktracker/releases/download/__RELEASE_TAG__/disktracker-__RELEASE_TAG__-windows-x64.zip"
$checksum64 = '__CHECKSUM_X64__'
$urlArm64 = "https://github.com/pratham15541/disktracker/releases/download/__RELEASE_TAG__/disktracker-__RELEASE_TAG__-windows-arm64.zip"
$checksumArm64 = '__CHECKSUM_ARM64__'

$isArm64 = $false
try {
  $isArm64 = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture -eq [System.Runtime.InteropServices.Architecture]::Arm64
} catch {
  $isArm64 = ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') -or ($env:PROCESSOR_ARCHITEW6432 -eq 'ARM64')
}

$packageArgs = @{
  packageName    = $packageName
  unzipLocation  = "$(Split-Path -parent $MyInvocation.MyCommand.Definition)"
  checksumType64 = 'sha256'
  fileType       = 'zip'
}

if ($isArm64) {
  $packageArgs.url64bit = $urlArm64
  $packageArgs.checksum64 = $checksumArm64
} else {
  $packageArgs.url64bit = $url64
  $packageArgs.checksum64 = $checksum64
}

Install-ChocolateyZipPackage @packageArgs
